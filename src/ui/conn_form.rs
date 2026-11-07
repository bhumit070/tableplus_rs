//! New / edit connection form (design.md, screen 04), shown in a dialog:
//! engine tiles decide the fields, a pasted connection URL fills them in,
//! the environment is a segmented control, SSH collapses to its header, and
//! Test connection reports inline (server version and latency, or the
//! error with details).

use std::rc::Rc;
use std::time::Instant;

use gpui::{
    AnyElement, App, Context, Entity, FocusHandle, Focusable, FontWeight, IntoElement,
    ParentElement, PathPromptOptions, Render, SharedString, Styled, Subscription, Window, div,
    prelude::*,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::checkbox::Checkbox;
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::select::{Select, SelectState};
use gpui_component::spinner::Spinner;
use gpui_component::switch::Switch;
use gpui_component::{
    ActiveTheme as _, Disableable as _, Icon, IndexPath, Sizable as _, WindowExt as _, h_flex,
    v_flex,
};
use gpui_kit_assets::IconName;

use super::dialog;
use super::theme::{dp, engine_badge, env_color, keychain_name, tokens};
use crate::db::ssh::{SshAuth, SshConfig, UnknownHostKey};
use crate::db::{Connection, ConnectionConfig, Driver, EnvTag, SafeMode, url};
use crate::runtime;

const SSH_AUTHS: [(SshAuth, &str); 3] = [
    (SshAuth::Password, "Password"),
    (SshAuth::Key, "Key file"),
    (SshAuth::Agent, "Agent"),
];

const ENVS: [(EnvTag, &str); 4] = [
    (EnvTag::Local, "Local"),
    (EnvTag::Development, "Dev"),
    (EnvTag::Staging, "Staging"),
    (EnvTag::Production, "Prod"),
];

/// One line under the safe-mode select. Automatic says what it does for
/// the chosen environment.
fn safe_mode_note(m: SafeMode, tag: EnvTag) -> String {
    match m {
        SafeMode::Auto if tag == EnvTag::Production => {
            "Confirm writes, because the environment is Production.".into()
        }
        SafeMode::Auto => "Off here; confirms writes when the environment is Production.".into(),
        SafeMode::Off => "Writes and commits run without asking.".into(),
        SafeMode::ConfirmWrites => {
            "Shows the SQL and asks before anything that writes, and before commits.".into()
        }
        SafeMode::ConfirmAll => "Shows the SQL and asks before every query.".into(),
        SafeMode::ReadOnly => {
            "Refuses writes; the server session is read-only where possible.".into()
        }
    }
}

/// What Save / Save & connect do with the form's connection.
pub type OnDone = Rc<dyn Fn(ConnectionConfig, bool, &mut Window, &mut App)>;

/// The last Test connection result.
#[derive(Clone)]
enum Tested {
    Ok(String),
    Failed { summary: String, details: String },
}

pub struct ConnForm {
    id: String,
    is_new: bool,
    driver: Driver,
    tag: EnvTag,
    url: Entity<InputState>,
    /// The URL field's length after the last change, to tell a paste.
    url_len: usize,
    /// From the URL field: an error, or a note that fields were filled in.
    url_note: Option<(bool, String)>,
    name: Entity<InputState>,
    host: Entity<InputState>,
    port: Entity<InputState>,
    user: Entity<InputState>,
    password: Entity<InputState>,
    database: Entity<InputState>,
    file: Entity<InputState>,
    ssl: bool,
    ssl_verify: bool,
    safe_mode: Entity<SelectState<Vec<&'static str>>>,
    ssh: bool,
    ssh_host: Entity<InputState>,
    ssh_port: Entity<InputState>,
    ssh_user: Entity<InputState>,
    ssh_auth: SshAuth,
    ssh_key: Entity<InputState>,
    ssh_secret: Entity<InputState>,
    /// From the last test: a new SSH host key the user can trust.
    unknown_host: Option<UnknownHostKey>,
    testing: bool,
    tested: Option<Tested>,
    show_details: bool,
    on_done: OnDone,
    focus: FocusHandle,
    _subs: Vec<Subscription>,
}

impl ConnForm {
    /// Opens the form in a dialog. `on_done` gets the connection and whether
    /// to connect too.
    pub fn open(
        cfg: ConnectionConfig,
        is_new: bool,
        on_done: OnDone,
        window: &mut Window,
        cx: &mut App,
    ) {
        let form = cx.new(|cx| ConnForm::new(cfg, is_new, on_done, window, cx));
        let w = dialog::width(680., cx);
        let first = std::cell::Cell::new(true);
        window.open_dialog(cx, move |d, window, cx| {
            if first.replace(false) {
                let input = form.read(cx).first_input();
                input.update(cx, |i, cx| i.focus(window, cx));
            }
            dialog::shell(d, w, cx).child(form.clone())
        });
    }

    pub fn new(
        cfg: ConnectionConfig,
        is_new: bool,
        on_done: OnDone,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let input = |v: &str, ph: &str, window: &mut Window, cx: &mut Context<Self>| {
            let (v, ph) = (v.to_string(), ph.to_string());
            cx.new(|cx| InputState::new(window, cx).default_value(v).placeholder(ph))
        };
        let port_s = if cfg.port == 0 {
            String::new()
        } else {
            cfg.port.to_string()
        };
        let url = input("", "postgres://user@host:5432/database", window, cx);
        let name = input(&cfg.name, "My database", window, cx);
        let host = input(&cfg.host, "127.0.0.1", window, cx);
        let port = input(&port_s, "port", window, cx);
        let user = input(&cfg.user, "user", window, cx);
        let password = cx.new(|cx| {
            InputState::new(window, cx)
                .masked(true)
                .default_value(cfg.password.clone())
                .placeholder("password")
        });
        let database = input(&cfg.database, "optional", window, cx);
        let file = input(&cfg.file, "/path/to/database.sqlite", window, cx);
        let si = SafeMode::ALL
            .iter()
            .position(|m| *m == cfg.safe_mode)
            .unwrap_or(0);
        let safe_mode = cx.new(|cx| {
            SelectState::new(
                SafeMode::ALL.iter().map(|m| m.label()).collect(),
                Some(IndexPath::new(si)),
                window,
                cx,
            )
        });
        let ssh = cfg.ssh.clone().unwrap_or_default();
        let ssh_host = input(&ssh.host, "bastion.example.com", window, cx);
        let ssh_port = input(
            &if ssh.port == 0 {
                "22".into()
            } else {
                ssh.port.to_string()
            },
            "22",
            window,
            cx,
        );
        let ssh_user = input(&ssh.user, "ssh user", window, cx);
        let ssh_key = input(
            &ssh.key_path,
            "~/.ssh/id_ed25519 (empty = default keys)",
            window,
            cx,
        );
        let ssh_secret = cx.new(|cx| {
            InputState::new(window, cx)
                .masked(true)
                .default_value(ssh.secret.clone())
        });

        let subs = vec![
            // Filled in on a paste (the text jumps by more than a character),
            // Enter or leaving the field; never halfway through typing.
            cx.subscribe_in(&url, window, |this, url, ev: &InputEvent, window, cx| {
                let len = url.read(cx).value().len();
                let grew = len > this.url_len + 1;
                this.url_len = len;
                match ev {
                    InputEvent::Change if grew => this.apply_url(window, cx),
                    InputEvent::Change if len == 0 => {
                        this.url_note = None;
                        cx.notify();
                    }
                    InputEvent::PressEnter { .. } | InputEvent::Blur => this.apply_url(window, cx),
                    _ => {}
                }
            }),
            cx.observe(&safe_mode, |_, _, cx| cx.notify()),
        ];

        Self {
            id: cfg.id,
            is_new,
            driver: cfg.driver,
            tag: cfg.tag,
            url,
            url_len: 0,
            url_note: None,
            name,
            host,
            port,
            user,
            password,
            database,
            file,
            ssl: cfg.ssl,
            ssl_verify: cfg.ssl_verify,
            safe_mode,
            ssh: cfg.ssh.is_some(),
            ssh_host,
            ssh_port,
            ssh_user,
            ssh_auth: ssh.auth,
            ssh_key,
            ssh_secret,
            unknown_host: None,
            testing: false,
            tested: None,
            show_details: false,
            on_done,
            focus: cx.focus_handle(),
            _subs: subs,
        }
    }

    /// The URL field for a new connection, the name when editing.
    fn first_input(&self) -> Entity<InputState> {
        if self.is_new {
            self.url.clone()
        } else {
            self.name.clone()
        }
    }

    fn set_driver(&mut self, d: Driver, window: &mut Window, cx: &mut Context<Self>) {
        // Swap the port only if it still holds some driver's default.
        let cur: u16 = self.port.read(cx).value().parse().unwrap_or(0);
        if cur == 0 || Driver::ALL.iter().any(|x| x.default_port() == cur) {
            let p = if d.default_port() == 0 {
                String::new()
            } else {
                d.default_port().to_string()
            };
            self.port.update(cx, |i, cx| i.set_value(p, window, cx));
        }
        self.driver = d;
        cx.notify();
    }

    /// Fills the fields from the URL field as soon as it holds a URL.
    fn apply_url(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.url.read(cx).value().trim().to_string();
        if text.is_empty() {
            return;
        }
        let p = match url::parse(&text) {
            Ok(p) => p,
            Err(e) => {
                self.url_note = Some((false, format!("{e:#}")));
                cx.notify();
                return;
            }
        };
        if let Some(d) = p.driver {
            self.set_driver(d, window, cx);
        }
        let set =
            |e: &Entity<InputState>, v: String, window: &mut Window, cx: &mut Context<Self>| {
                e.update(cx, |i, cx| i.set_value(v, window, cx));
            };
        if p.driver == Some(Driver::Sqlite) {
            set(&self.file, p.file.clone(), window, cx);
            if self.name.read(cx).value().is_empty() {
                let stem = std::path::Path::new(&p.file)
                    .file_stem()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_default();
                set(&self.name, stem, window, cx);
            }
        } else {
            set(&self.host, p.host.clone(), window, cx);
            if let Some(port) = p.port {
                set(&self.port, port.to_string(), window, cx);
            }
            set(&self.user, p.user.clone(), window, cx);
            if !p.password.is_empty() {
                set(&self.password, p.password.clone(), window, cx);
            }
            set(&self.database, p.database.clone(), window, cx);
            self.ssl = p.ssl;
            self.ssl_verify = p.ssl_verify;
            if self.name.read(cx).value().is_empty() {
                let name = if p.database.is_empty() || self.driver == Driver::Redis {
                    p.host.clone()
                } else {
                    p.database.clone()
                };
                set(&self.name, name, window, cx);
            }
        }
        let where_ = if p.password.is_empty() {
            String::new()
        } else {
            format!("; the password will be saved in {}", keychain_name())
        };
        self.url_note = Some((true, format!("Filled in from the URL{where_}.")));
        // The URL may hold a password: don't leave it on screen.
        self.url_len = 0;
        self.url.update(cx, |i, cx| i.set_value("", window, cx));
        cx.notify();
    }

    pub fn config(&self, cx: &App) -> ConnectionConfig {
        let v = |e: &Entity<InputState>| e.read(cx).value().trim().to_string();
        let driver = self.driver;
        let sm_label = self
            .safe_mode
            .read(cx)
            .selected_value()
            .copied()
            .unwrap_or("");
        ConnectionConfig {
            id: self.id.clone(),
            name: v(&self.name),
            driver,
            tag: self.tag,
            host: v(&self.host),
            port: v(&self.port).parse().unwrap_or(driver.default_port()),
            user: v(&self.user),
            database: v(&self.database),
            file: v(&self.file),
            ssl: self.ssl,
            ssl_verify: self.ssl && self.ssl_verify,
            password: self.password.read(cx).value().to_string(),
            safe_mode: SafeMode::ALL
                .into_iter()
                .find(|m| m.label() == sm_label)
                .unwrap_or_default(),
            ssh: (self.ssh && driver != Driver::Sqlite).then(|| SshConfig {
                host: v(&self.ssh_host),
                port: v(&self.ssh_port).parse().unwrap_or(22),
                user: v(&self.ssh_user),
                auth: self.ssh_auth,
                key_path: v(&self.ssh_key),
                secret: self.ssh_secret.read(cx).value().to_string(),
            }),
        }
    }

    fn done(&mut self, connect: bool, window: &mut Window, cx: &mut Context<Self>) {
        let cfg = self.config(cx);
        window.close_dialog(cx);
        (self.on_done)(cfg, connect, window, cx);
    }

    /// Accepts the SSH host key the last test reported, then tests again.
    fn trust_host(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(u) = self.unknown_host.take()
            && let Err(e) = u.trust()
        {
            self.tested = Some(Tested::Failed {
                summary: "Couldn't save the host key".into(),
                details: format!("{e:#}"),
            });
            cx.notify();
            return;
        }
        self.test(window, cx);
    }

    fn browse_key(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Choose SSH private key".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = rx.await else {
                return;
            };
            let Some(p) = paths.into_iter().next() else {
                return;
            };
            this.update_in(cx, |this, window, cx| {
                let path = p.to_string_lossy().to_string();
                this.ssh_key
                    .update(cx, |i, cx| i.set_value(path, window, cx));
            })
            .ok();
        })
        .detach();
    }

    pub fn test(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let cfg = self.config(cx);
        self.testing = true;
        self.tested = None;
        self.show_details = false;
        self.unknown_host = None;
        cx.notify();
        let task = runtime::spawn(async move {
            let started = Instant::now();
            let c = Connection::connect(&cfg).await?;
            let version = c.server_version(cfg.driver).await.unwrap_or_default();
            let ms = started.elapsed().as_millis();
            let db = c.current_database();
            c.close().await;
            Ok((version, ms, db))
        });
        cx.spawn_in(window, async move |this, cx| {
            let res = task.await;
            this.update(cx, |this, cx| {
                this.testing = false;
                this.tested = Some(match res {
                    Ok((version, ms, db)) => {
                        let mut parts = vec![];
                        parts.push(if version.is_empty() {
                            "Connected".to_string()
                        } else {
                            format!("Connected · {} {version}", this.driver.label())
                        });
                        if !db.is_empty() {
                            parts.push(db);
                        }
                        parts.push(format!("{ms} ms"));
                        Tested::Ok(parts.join(" · "))
                    }
                    Err(e) => {
                        this.unknown_host = e.downcast_ref::<UnknownHostKey>().cloned();
                        let details = format!("{e:#}");
                        let summary = if this.unknown_host.is_some() {
                            "Unknown SSH host key".to_string()
                        } else {
                            details
                                .split(": ")
                                .next()
                                .unwrap_or(&details)
                                .lines()
                                .next()
                                .unwrap_or_default()
                                .to_string()
                        };
                        Tested::Failed { summary, details }
                    }
                });
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn browse(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Open SQLite database".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = rx.await else {
                return;
            };
            let Some(p) = paths.into_iter().next() else {
                return;
            };
            this.update_in(cx, |this, window, cx| {
                let path = p.to_string_lossy().to_string();
                this.file.update(cx, |i, cx| i.set_value(path, window, cx));
                if this.name.read(cx).value().is_empty() {
                    let n = p
                        .file_stem()
                        .map(|s| s.to_string_lossy().to_string())
                        .unwrap_or_default();
                    this.name.update(cx, |i, cx| i.set_value(n, window, cx));
                }
            })
            .ok();
        })
        .detach();
    }

    fn safe_mode_value(&self, cx: &App) -> SafeMode {
        let l = self
            .safe_mode
            .read(cx)
            .selected_value()
            .copied()
            .unwrap_or("");
        SafeMode::ALL
            .into_iter()
            .find(|m| m.label() == l)
            .unwrap_or_default()
    }
}

/// A label over its field.
fn field(label: impl IntoElement, input: impl IntoElement, cx: &App) -> gpui::Div {
    let t = tokens(cx);
    v_flex()
        .gap(dp(6.))
        .min_w_0()
        .child(
            div()
                .text_size(dp(12.))
                .font_weight(FontWeight::MEDIUM)
                .text_color(t.text_secondary)
                .child(label),
        )
        .child(input)
}

fn hint(text: impl Into<SharedString>, color: gpui::Hsla) -> gpui::Div {
    div()
        .text_size(dp(11.5))
        .text_color(color)
        .child(text.into())
}

impl Focusable for ConnForm {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for ConnForm {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = tokens(cx);
        let driver = self.driver;
        let entity = cx.entity();

        let engines: Vec<AnyElement> = Driver::ALL
            .iter()
            .map(|&d| {
                let entity = entity.clone();
                let selected = d == driver;
                h_flex()
                    .id(SharedString::from(format!(
                        "engine-{}",
                        d.short().to_lowercase()
                    )))
                    .debug_selector(move || format!("engine-{}", d.short().to_lowercase()))
                    .flex_1()
                    .min_w_0()
                    .gap(dp(8.))
                    .px(dp(10.))
                    .h(dp(44.))
                    .rounded(dp(9.))
                    .border_1()
                    .cursor_pointer()
                    .map(|el| {
                        if selected {
                            el.border_color(t.accent).bg(t.accent.opacity(0.10))
                        } else {
                            el.border_color(t.border)
                                .bg(t.surface)
                                .hover(|s| s.bg(t.hover))
                        }
                    })
                    .child(engine_badge(d, 24., cx))
                    .child(
                        div()
                            .truncate()
                            .text_size(dp(12.))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(if selected { t.text } else { t.text_secondary })
                            .child(match d {
                                Driver::Postgres => "Postgres",
                                Driver::MySql => "MySQL",
                                d => d.label(),
                            }),
                    )
                    .on_click(move |_, window, cx| {
                        entity.update(cx, |this, cx| this.set_driver(d, window, cx))
                    })
                    .into_any_element()
            })
            .collect();

        let env_ix = ENVS.iter().position(|(e, _)| *e == self.tag).unwrap_or(0);
        let env = {
            let entity = entity.clone();
            dialog::segmented(
                "env",
                &ENVS.map(|(_, l)| l),
                env_ix,
                Rc::new(move |i, _, cx| {
                    entity.update(cx, |this, cx| {
                        this.tag = ENVS[i].0;
                        cx.notify();
                    })
                }),
                cx,
            )
        };
        let env_dot = div()
            .size(dp(8.))
            .rounded_full()
            .bg(env_color(self.tag, cx));

        let sm = self.safe_mode_value(cx);
        let safe = field(
            "Safe mode",
            v_flex()
                .gap(dp(6.))
                .child(Select::new(&self.safe_mode).cursor_pointer())
                .child(hint(safe_mode_note(sm, self.tag), t.text_muted)),
            cx,
        )
        .flex_1();
        let url_note = self.url_note.clone();
        let mut body = v_flex()
            .gap(dp(16.))
            .child(dialog::section(
                "Engine",
                h_flex().gap(dp(8.)).children(engines),
                cx,
            ))
            .child(
                v_flex()
                    .gap(dp(6.))
                    .child(
                        Input::new(&self.url)
                            .prefix(Icon::new(IconName::Link).small().text_color(t.text_muted)),
                    )
                    .child(match url_note {
                        Some((true, msg)) => hint(msg, t.success),
                        Some((false, msg)) => hint(msg, t.danger),
                        None => hint(
                            "Paste a connection URL (or type one and press ↵) to fill in the fields below.",
                            t.text_muted,
                        ),
                    }),
            )
            .child(
                h_flex()
                    .gap(dp(12.))
                    .items_start()
                    .child(field("Name", Input::new(&self.name), cx).flex_1())
                    .child(
                        field(
                            h_flex().gap(dp(6.)).child(env_dot).child("Environment"),
                            env,
                            cx,
                        )
                        .w(dp(280.)),
                    ),
            );

        if driver == Driver::Sqlite {
            body = body
                .child(field(
                    "File",
                    h_flex()
                        .gap(dp(8.))
                        .child(div().flex_1().child(Input::new(&self.file)))
                        .child(
                            Button::new("browse")
                                .cursor_pointer()
                                .label("Browse…")
                                .outline()
                                .on_click(
                                    cx.listener(|this, _, window, cx| this.browse(window, cx)),
                                ),
                        ),
                    cx,
                ))
                .child(safe);
        } else {
            body = body
                .child(
                    h_flex()
                        .gap(dp(12.))
                        .child(field("Host", Input::new(&self.host), cx).flex_1())
                        .child(field("Port", Input::new(&self.port), cx).w(dp(100.))),
                )
                .child(
                    h_flex()
                        .gap(dp(12.))
                        .items_start()
                        .child(field("User", Input::new(&self.user), cx).flex_1())
                        .child(
                            field(
                                "Password",
                                v_flex()
                                    .gap(dp(4.))
                                    .child(Input::new(&self.password).mask_toggle())
                                    .child(hint(
                                        format!("Saved in {}", keychain_name()),
                                        t.text_faint,
                                    )),
                                cx,
                            )
                            .flex_1(),
                        ),
                )
                .child(
                    h_flex()
                        .gap(dp(12.))
                        .items_start()
                        .child(
                            field(
                                if driver == Driver::Redis {
                                    "Database index"
                                } else {
                                    "Database"
                                },
                                Input::new(&self.database),
                                cx,
                            )
                            .flex_1(),
                        )
                        .child(safe),
                )
                .child(
                    h_flex()
                        .gap(dp(20.))
                        .child(
                            Checkbox::new("ssl")
                                .cursor_pointer()
                                .checked(self.ssl)
                                .label("Require SSL / TLS")
                                .on_click(cx.listener(|this, checked: &bool, _, cx| {
                                    this.ssl = *checked;
                                    cx.notify();
                                })),
                        )
                        .when(
                            self.ssl && matches!(driver, Driver::Postgres | Driver::MySql),
                            |f| {
                                f.child(
                                    Checkbox::new("ssl-verify")
                                        .cursor_pointer()
                                        .checked(self.ssl_verify)
                                        .label("Verify server certificate")
                                        .on_click(cx.listener(|this, checked: &bool, _, cx| {
                                            this.ssl_verify = *checked;
                                            cx.notify();
                                        })),
                                )
                            },
                        ),
                );
        }

        if driver != Driver::Sqlite {
            body = body.child(self.ssh_section(cx));
        }

        // The body scrolls and gives way first, so the title and footer stay
        // on screen in a short window.
        let scroll = div()
            .id("conn-form-body")
            .flex_1()
            .min_h_0()
            .max_h(dp(600.))
            .overflow_y_scroll()
            .pr(dp(4.))
            .child(body);

        let tested = self.tested.clone();
        let status: AnyElement = if self.testing {
            h_flex()
                .gap(dp(8.))
                .text_size(dp(12.))
                .text_color(t.text_muted)
                .child(Spinner::new().small())
                .child("Connecting…")
                .into_any_element()
        } else {
            match &tested {
                Some(Tested::Ok(msg)) => h_flex()
                    .gap(dp(6.))
                    .min_w_0()
                    .text_size(dp(12.))
                    .text_color(t.success)
                    .child(Icon::new(IconName::CircleCheck).small())
                    .child(div().truncate().child(msg.clone()))
                    .into_any_element(),
                Some(Tested::Failed { summary, .. }) => h_flex()
                    .gap(dp(6.))
                    .min_w_0()
                    .text_size(dp(12.))
                    .text_color(t.danger)
                    .child(Icon::new(IconName::CircleX).small())
                    .child(div().truncate().child(summary.clone()))
                    .child(
                        Button::new("test-details")
                            .cursor_pointer()
                            .label(if self.show_details { "Hide" } else { "Details" })
                            .ghost()
                            .xsmall()
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.show_details = !this.show_details;
                                cx.notify();
                            })),
                    )
                    .when(self.unknown_host.is_some(), |f| {
                        f.child(
                            Button::new("trust-host")
                                .cursor_pointer()
                                .label("Trust host")
                                .outline()
                                .xsmall()
                                .on_click(
                                    cx.listener(|this, _, window, cx| this.trust_host(window, cx)),
                                ),
                        )
                    })
                    .into_any_element(),
                None => div().into_any_element(),
            }
        };
        let details = match (&tested, self.show_details) {
            (Some(Tested::Failed { details, .. }), true) => Some(
                div()
                    .id("test-details-text")
                    .max_h(dp(120.))
                    .overflow_y_scroll()
                    .p(dp(10.))
                    .rounded(dp(8.))
                    .border_1()
                    .border_color(t.danger.opacity(0.35))
                    .bg(t.danger.opacity(0.08))
                    .font_family(cx.theme().mono_font_family.clone())
                    .text_size(dp(11.5))
                    .text_color(t.text_secondary)
                    .child(details.clone()),
            ),
            _ => None,
        };

        v_flex()
            .track_focus(&self.focus)
            .key_context("ConnForm")
            .min_h_0()
            .flex_shrink(1.)
            .gap(dp(16.))
            .on_action(
                cx.listener(|this, _: &super::RunQuery, window, cx| this.done(true, window, cx)),
            )
            .child(dialog::title(
                if self.is_new {
                    "New connection"
                } else {
                    "Edit connection"
                },
                None,
                cx,
            ))
            .child(scroll)
            .children(details)
            .child(
                h_flex()
                    .pt(dp(14.))
                    .gap(dp(8.))
                    .border_t_1()
                    .border_color(t.border)
                    .child(
                        Button::new("test")
                            .cursor_pointer()
                            .icon(IconName::Zap)
                            .label("Test connection")
                            .outline()
                            .disabled(self.testing)
                            .on_click(cx.listener(|this, _, window, cx| this.test(window, cx))),
                    )
                    .child(div().flex_1().min_w_0().overflow_hidden().child(status))
                    .child(
                        Button::new("cancel")
                            .cursor_pointer()
                            .label("Cancel")
                            .ghost()
                            .on_click(|_, w, cx| w.close_dialog(cx)),
                    )
                    .child(
                        div().debug_selector(|| "conn-save".into()).child(
                            Button::new("save")
                                .cursor_pointer()
                                .label("Save")
                                .outline()
                                .on_click(
                                    cx.listener(|this, _, window, cx| this.done(false, window, cx)),
                                ),
                        ),
                    )
                    .child(
                        Button::new("connect")
                            .cursor_pointer()
                            .label("Save & connect ⌘↵")
                            .primary()
                            .on_click(
                                cx.listener(|this, _, window, cx| this.done(true, window, cx)),
                            ),
                    ),
            )
    }
}

impl ConnForm {
    /// SSH tunnel: a header with a switch; the fields only while it's on.
    fn ssh_section(&self, cx: &mut Context<Self>) -> AnyElement {
        let t = tokens(cx);
        let entity = cx.entity();
        let header = h_flex()
            .gap(dp(10.))
            .child(
                Icon::new(IconName::Network)
                    .small()
                    .text_color(t.text_muted),
            )
            .child(
                v_flex()
                    .child(
                        div()
                            .text_size(dp(12.5))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(t.text)
                            .child("SSH tunnel"),
                    )
                    .child(hint("Connect through a bastion host", t.text_muted)),
            )
            .child(div().flex_1())
            .child(Switch::new("ssh").checked(self.ssh).on_click(cx.listener(
                |this, checked: &bool, _, cx| {
                    this.ssh = *checked;
                    cx.notify();
                },
            )));
        let mut card = v_flex()
            .gap(dp(14.))
            .p(dp(14.))
            .rounded(dp(10.))
            .border_1()
            .border_color(t.border)
            .bg(t.canvas.opacity(0.5))
            .child(header);
        if self.ssh {
            let auth = self.ssh_auth;
            let ai = SSH_AUTHS.iter().position(|(a, _)| *a == auth).unwrap_or(0);
            card = card
                .child(
                    h_flex()
                        .gap(dp(12.))
                        .child(field("SSH host", Input::new(&self.ssh_host), cx).flex_1())
                        .child(field("Port", Input::new(&self.ssh_port), cx).w(dp(100.))),
                )
                .child(
                    h_flex()
                        .gap(dp(12.))
                        .items_end()
                        .child(field("SSH user", Input::new(&self.ssh_user), cx).flex_1())
                        .child(
                            field(
                                "Authentication",
                                dialog::segmented(
                                    "ssh-auth",
                                    &SSH_AUTHS.map(|(_, l)| l),
                                    ai,
                                    Rc::new(move |i, _, cx| {
                                        entity.update(cx, |this, cx| {
                                            this.ssh_auth = SSH_AUTHS[i].0;
                                            cx.notify();
                                        })
                                    }),
                                    cx,
                                ),
                                cx,
                            )
                            .w(dp(260.)),
                        ),
                )
                .when(auth == SshAuth::Key, |f| {
                    f.child(field(
                        "Key file",
                        h_flex()
                            .gap(dp(8.))
                            .child(div().flex_1().child(Input::new(&self.ssh_key)))
                            .child(
                                Button::new("browse-key")
                                    .cursor_pointer()
                                    .label("Browse…")
                                    .outline()
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.browse_key(window, cx)
                                    })),
                            ),
                        cx,
                    ))
                })
                .when(auth != SshAuth::Agent, |f| {
                    f.child(field(
                        if auth == SshAuth::Key {
                            "Passphrase"
                        } else {
                            "SSH password"
                        },
                        Input::new(&self.ssh_secret).mask_toggle(),
                        cx,
                    ))
                })
                .child(hint(
                    "Host and port above are as seen from the SSH server, e.g. 127.0.0.1.",
                    t.text_muted,
                ));
        }
        card.into_any_element()
    }
}
