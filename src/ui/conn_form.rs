//! New / edit connection form, shown inside a dialog.

use gpui::{
    App, Context, Entity, IntoElement, ParentElement, PathPromptOptions, Render, Styled,
    Subscription, Window, div, prelude::*, px,
};
use gpui_component::button::Button;
use gpui_component::checkbox::Checkbox;
use gpui_component::input::{Input, InputState};
use gpui_component::select::{Select, SelectEvent, SelectState};
use gpui_component::{ActiveTheme as _, IndexPath, Sizable as _, h_flex, v_flex};

use crate::db::ssh::{SshAuth, SshConfig, UnknownHostKey};
use crate::db::{Connection, ConnectionConfig, Driver, EnvTag, SafeMode};

const SSH_AUTHS: [(SshAuth, &str); 3] = [
    (SshAuth::Password, "Password"),
    (SshAuth::Key, "Private key"),
    (SshAuth::Agent, "SSH agent"),
];
use crate::runtime;

pub struct ConnForm {
    id: String,
    driver: Entity<SelectState<Vec<&'static str>>>,
    tag: Entity<SelectState<Vec<&'static str>>>,
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
    ssh_auth: Entity<SelectState<Vec<&'static str>>>,
    ssh_key: Entity<InputState>,
    ssh_secret: Entity<InputState>,
    /// From the last test: a new SSH host key the user can trust.
    unknown_host: Option<UnknownHostKey>,
    testing: bool,
    pub status: Option<(bool, String)>,
    _subs: Vec<Subscription>,
}

impl ConnForm {
    pub fn new(cfg: ConnectionConfig, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = |v: &str, ph: &str, window: &mut Window, cx: &mut Context<Self>| {
            let (v, ph) = (v.to_string(), ph.to_string());
            cx.new(|cx| InputState::new(window, cx).default_value(v).placeholder(ph))
        };
        let drivers: Vec<&'static str> = Driver::ALL.iter().map(|d| d.label()).collect();
        let di = Driver::ALL
            .iter()
            .position(|d| *d == cfg.driver)
            .unwrap_or(0);
        let driver = cx.new(|cx| SelectState::new(drivers, Some(IndexPath::new(di)), window, cx));
        let tags: Vec<&'static str> = EnvTag::ALL.iter().map(|t| t.label()).collect();
        let ti = EnvTag::ALL.iter().position(|t| *t == cfg.tag).unwrap_or(0);
        let tag = cx.new(|cx| SelectState::new(tags, Some(IndexPath::new(ti)), window, cx));
        let port_s = if cfg.port == 0 {
            String::new()
        } else {
            cfg.port.to_string()
        };
        let name = input(&cfg.name, "My database", window, cx);
        let host = input(
            &cfg.host,
            "127.0.0.1 (or a mongodb:// URI for MongoDB)",
            window,
            cx,
        );
        let port = input(&port_s, "port", window, cx);
        let user = input(&cfg.user, "user", window, cx);
        let password = cx.new(|cx| {
            InputState::new(window, cx)
                .masked(true)
                .default_value(cfg.password.clone())
                .placeholder("password")
        });
        let database = input(&cfg.database, "database (optional)", window, cx);
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
        let ssh_host = input(&ssh.host, "ssh.example.com", window, cx);
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
        let ai = SSH_AUTHS
            .iter()
            .position(|(a, _)| *a == ssh.auth)
            .unwrap_or(0);
        let ssh_auth = cx.new(|cx| {
            SelectState::new(
                SSH_AUTHS.iter().map(|(_, l)| *l).collect(),
                Some(IndexPath::new(ai)),
                window,
                cx,
            )
        });
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
            cx.subscribe_in(
                &ssh_auth,
                window,
                |_, _, _: &SelectEvent<Vec<&'static str>>, _, cx| cx.notify(),
            ),
            cx.subscribe_in(
                &driver,
                window,
                |this, _, ev: &SelectEvent<Vec<&'static str>>, window, cx| {
                    let SelectEvent::Confirm(Some(label)) = ev else {
                        return;
                    };
                    let Some(d) = Driver::ALL.into_iter().find(|d| d.label() == *label) else {
                        return;
                    };
                    // Swap the port only if it still holds some driver's default.
                    let cur: u16 = this.port.read(cx).value().parse().unwrap_or(0);
                    if cur == 0 || Driver::ALL.iter().any(|x| x.default_port() == cur) {
                        let p = if d.default_port() == 0 {
                            String::new()
                        } else {
                            d.default_port().to_string()
                        };
                        this.port.update(cx, |i, cx| i.set_value(p, window, cx));
                    }
                    cx.notify();
                },
            ),
        ];

        Self {
            id: cfg.id,
            driver,
            tag,
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
            ssh_auth,
            ssh_key,
            ssh_secret,
            unknown_host: None,
            testing: false,
            status: None,
            _subs: subs,
        }
    }

    fn driver(&self, cx: &App) -> Driver {
        let label = self.driver.read(cx).selected_value().copied().unwrap_or("");
        Driver::ALL
            .into_iter()
            .find(|d| d.label() == label)
            .unwrap_or(Driver::Postgres)
    }

    pub fn config(&self, cx: &App) -> ConnectionConfig {
        let v = |e: &Entity<InputState>| e.read(cx).value().trim().to_string();
        let driver = self.driver(cx);
        let tag_label = self.tag.read(cx).selected_value().copied().unwrap_or("");
        ConnectionConfig {
            id: self.id.clone(),
            name: v(&self.name),
            driver,
            tag: EnvTag::ALL
                .into_iter()
                .find(|t| t.label() == tag_label)
                .unwrap_or_default(),
            host: v(&self.host),
            port: v(&self.port).parse().unwrap_or(driver.default_port()),
            user: v(&self.user),
            database: v(&self.database),
            file: v(&self.file),
            ssl: self.ssl,
            ssl_verify: self.ssl && self.ssl_verify,
            password: self.password.read(cx).value().to_string(),
            safe_mode: {
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
            },
            ssh: (self.ssh && driver != Driver::Sqlite).then(|| SshConfig {
                host: v(&self.ssh_host),
                port: v(&self.ssh_port).parse().unwrap_or(22),
                user: v(&self.ssh_user),
                auth: self.ssh_auth(cx),
                key_path: v(&self.ssh_key),
                secret: self.ssh_secret.read(cx).value().to_string(),
            }),
        }
    }

    fn ssh_auth(&self, cx: &App) -> SshAuth {
        let label = self
            .ssh_auth
            .read(cx)
            .selected_value()
            .copied()
            .unwrap_or("");
        SSH_AUTHS
            .iter()
            .find(|(_, l)| *l == label)
            .map(|(a, _)| *a)
            .unwrap_or_default()
    }

    /// Accepts the SSH host key the last test reported, then tests again.
    fn trust_host(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(u) = self.unknown_host.take()
            && let Err(e) = u.trust()
        {
            self.status = Some((false, format!("Couldn't save the host key: {e:#}")));
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
        self.status = None;
        self.unknown_host = None;
        cx.notify();
        let task = runtime::spawn(async move {
            let c = Connection::connect(&cfg).await?;
            let db = c.current_database();
            c.close().await;
            Ok(db)
        });
        cx.spawn_in(window, async move |this, cx| {
            let res = task.await;
            this.update(cx, |this, cx| {
                this.testing = false;
                this.status = Some(match res {
                    Ok(db) if db.is_empty() => (true, "Connection OK".into()),
                    Ok(db) => (true, format!("Connection OK · database: {db}")),
                    Err(e) => {
                        this.unknown_host = e.downcast_ref::<UnknownHostKey>().cloned();
                        (false, format!("{e:#}"))
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
}

fn row(label: &'static str, field: impl IntoElement, cx: &App) -> impl IntoElement {
    h_flex()
        .gap_3()
        .child(
            div()
                .w(px(84.))
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(label),
        )
        .child(div().flex_1().child(field))
}

impl Render for ConnForm {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let driver = self.driver(cx);
        let theme = cx.theme().clone();
        v_flex()
            .gap_2()
            .child(row("Type", Select::new(&self.driver).small(), cx))
            .child(row("Name", Input::new(&self.name).small(), cx))
            .child(row("Tag", Select::new(&self.tag).small(), cx))
            .child(row("Safe mode", Select::new(&self.safe_mode).small(), cx))
            .when(driver == Driver::Sqlite, |f| {
                f.child(row(
                    "File",
                    h_flex()
                        .gap_1()
                        .child(div().flex_1().child(Input::new(&self.file).small()))
                        .child(
                            Button::new("browse").label("Browse…").small().on_click(
                                cx.listener(|this, _, window, cx| this.browse(window, cx)),
                            ),
                        ),
                    cx,
                ))
            })
            .when(driver != Driver::Sqlite, |f| {
                f.child(row(
                    "Host",
                    h_flex()
                        .gap_1()
                        .child(div().flex_1().child(Input::new(&self.host).small()))
                        .child(div().w(px(90.)).child(Input::new(&self.port).small())),
                    cx,
                ))
                .child(row("User", Input::new(&self.user).small(), cx))
                .child(row(
                    "Password",
                    Input::new(&self.password).small().mask_toggle(),
                    cx,
                ))
                .child(row(
                    if driver == Driver::Redis {
                        "DB index"
                    } else {
                        "Database"
                    },
                    Input::new(&self.database).small(),
                    cx,
                ))
                .child(row(
                    "SSL / TLS",
                    Checkbox::new("ssl")
                        .checked(self.ssl)
                        .label("Require encrypted connection")
                        .on_click(cx.listener(|this, checked: &bool, _, cx| {
                            this.ssl = *checked;
                            cx.notify();
                        })),
                    cx,
                ))
                .when(
                    self.ssl && matches!(driver, Driver::Postgres | Driver::MySql),
                    |f| {
                        f.child(row(
                            "",
                            Checkbox::new("ssl-verify")
                                .checked(self.ssl_verify)
                                .label("Verify server certificate (recommended)")
                                .on_click(cx.listener(|this, checked: &bool, _, cx| {
                                    this.ssl_verify = *checked;
                                    cx.notify();
                                })),
                            cx,
                        ))
                    },
                )
            })
            .when(driver != Driver::Sqlite, |f| {
                f.child(row(
                    "SSH",
                    Checkbox::new("ssh")
                        .checked(self.ssh)
                        .label("Connect through an SSH tunnel")
                        .on_click(cx.listener(|this, checked: &bool, _, cx| {
                            this.ssh = *checked;
                            cx.notify();
                        })),
                    cx,
                ))
            })
            .when(self.ssh && driver != Driver::Sqlite, |f| {
                let auth = self.ssh_auth(cx);
                f.child(row(
                    "SSH host",
                    h_flex()
                        .gap_1()
                        .child(div().flex_1().child(Input::new(&self.ssh_host).small()))
                        .child(div().w(px(90.)).child(Input::new(&self.ssh_port).small())),
                    cx,
                ))
                .child(row("SSH user", Input::new(&self.ssh_user).small(), cx))
                .child(row("Auth", Select::new(&self.ssh_auth).small(), cx))
                .when(auth == SshAuth::Key, |f| {
                    f.child(row(
                        "Key file",
                        h_flex()
                            .gap_1()
                            .child(div().flex_1().child(Input::new(&self.ssh_key).small()))
                            .child(Button::new("browse-key").label("Browse…").small().on_click(
                                cx.listener(|this, _, window, cx| this.browse_key(window, cx)),
                            )),
                        cx,
                    ))
                })
                .when(auth != SshAuth::Agent, |f| {
                    f.child(row(
                        if auth == SshAuth::Key {
                            "Passphrase"
                        } else {
                            "SSH password"
                        },
                        Input::new(&self.ssh_secret).small().mask_toggle(),
                        cx,
                    ))
                })
                .child(row(
                    "",
                    div().text_xs().text_color(theme.muted_foreground).child(
                        "Host and port above are as seen from the SSH server, e.g. 127.0.0.1.",
                    ),
                    cx,
                ))
            })
            .child(
                h_flex()
                    .gap_2()
                    .pt_1()
                    .child(
                        Button::new("test")
                            .label("Test")
                            .small()
                            .loading(self.testing)
                            .on_click(cx.listener(|this, _, window, cx| this.test(window, cx))),
                    )
                    .when_some(self.status.clone(), |f, (ok, msg)| {
                        f.child(
                            div()
                                .flex_1()
                                .text_xs()
                                .text_color(if ok { theme.success } else { theme.danger })
                                .child(msg),
                        )
                    })
                    .when(self.unknown_host.is_some(), |f| {
                        f.child(
                            Button::new("trust-host")
                                .label("Trust Host")
                                .small()
                                .on_click(
                                    cx.listener(|this, _, window, cx| this.trust_host(window, cx)),
                                ),
                        )
                    }),
            )
    }
}
