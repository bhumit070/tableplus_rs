//! Connections home (design.md, screen 03): saved connections as cards,
//! grouped Open now / Saved, with search (`/`), an environment filter,
//! Import and New connection.

use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use chrono::Utc;
use gpui::{
    AnyElement, App, ClickEvent, Context, Div, Entity, FocusHandle, Focusable, FontWeight, Hsla,
    InteractiveElement, IntoElement, KeyDownEvent, ParentElement, PathPromptOptions, Render,
    Styled, Subscription, WeakEntity, Window, div, prelude::*, px,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::dialog::{DialogButtonProps, DialogFooter};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::menu::{ContextMenuExt as _, DropdownMenu as _, PopupMenuItem};
use gpui_component::notification::Notification;
use gpui_component::spinner::Spinner;
use gpui_component::{ActiveTheme as _, Icon, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit_assets::IconName;
use uuid::Uuid;

use super::app::{AppView, shortcut_hint};
use super::conn_form::{ConnForm, OnDone};
use super::dialog;
use super::theme::{caps, dp, engine_badge, env_pill, kbd, keychain_name, safe_mode_badge, tokens};
use crate::db::{ConnectionConfig, Driver, EnvTag, SafeMode};
use crate::{import, settings, store};

pub struct Home {
    app: WeakEntity<AppView>,
    pub connections: Vec<ConnectionConfig>,
    search: Entity<InputState>,
    /// Ids of connections being opened (several at launch).
    pub connecting: HashSet<String>,
    /// Why the last attempt to open a connection failed, by id.
    failed: HashMap<String, String>,
    /// Set when connections.json couldn't be read (it's kept aside).
    load_warning: Option<String>,
    env_filter: Option<EnvTag>,
    focus: FocusHandle,
    _subs: Vec<Subscription>,
}

impl Home {
    pub fn new(app: WeakEntity<AppView>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search connections…"));
        let subs = vec![
            cx.subscribe_in(&search, window, |_, _, ev: &InputEvent, _, cx| {
                if let InputEvent::Change = ev {
                    cx.notify();
                }
            }),
        ];
        let (connections, load_warning) = store::load();
        Self {
            app,
            connections,
            load_warning,
            search,
            connecting: Default::default(),
            failed: Default::default(),
            env_filter: None,
            focus: cx.focus_handle(),
            _subs: subs,
        }
    }

    fn persist(&self, window: &mut Window, cx: &mut Context<Self>) {
        if let Err(e) = store::save_all(&self.connections) {
            window.push_notification(
                Notification::error(format!("Couldn't save connections: {e:#}")),
                cx,
            );
        }
    }

    fn upsert(&mut self, cfg: &ConnectionConfig, window: &mut Window, cx: &mut Context<Self>) {
        if let Err(e) = store::save_secrets(cfg) {
            window.push_notification(
                Notification::warning(format!("Couldn't store password in Keychain: {e:#}")),
                cx,
            );
        }
        match self.connections.iter_mut().find(|c| c.id == cfg.id) {
            Some(c) => *c = cfg.clone(),
            None => self.connections.push(cfg.clone()),
        }
        self.persist(window, cx);
        cx.notify();
    }

    pub fn open_form(
        &mut self,
        cfg: Option<ConnectionConfig>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let is_new = cfg.is_none();
        let mut cfg = cfg.unwrap_or_else(|| {
            let mut c = ConnectionConfig::new(Driver::Postgres);
            c.safe_mode = settings::get(cx).default_safe_mode;
            c
        });
        // The form must show the real secret: a blank field saved back
        // would delete the stored password.
        if !is_new && let Err(e) = store::load_secrets(&mut cfg) {
            window.push_notification(
                Notification::error(format!("{e:#}")).title("Can't edit this connection right now"),
                cx,
            );
            return;
        }
        let home = cx.entity().downgrade();
        let on_done: OnDone = Rc::new(move |cfg, connect, window, cx| {
            home.update(cx, |h, cx| {
                h.upsert(&cfg, window, cx);
                if connect {
                    h.connect(cfg.clone(), window, cx);
                }
            })
            .ok();
        });
        ConnForm::open(cfg, is_new, on_done, window, cx);
    }

    pub fn connect(
        &mut self,
        mut cfg: ConnectionConfig,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // A keychain that can't be read (locked, no Secret Service on
        // Linux) mustn't block connections that need no password: try
        // without it, and say so.
        let keychain_failed = match store::load_secrets(&mut cfg) {
            Ok(()) => false,
            Err(e) => {
                window.push_notification(
                    Notification::warning(format!("{e:#}. Connecting without the saved password.")),
                    cx,
                );
                true
            }
        };
        self.connecting.insert(cfg.id.clone());
        self.failed.remove(&cfg.id);
        cx.notify();
        let home = cx.entity().downgrade();
        self.app
            .update(cx, |app, cx| {
                let retry = cfg.clone();
                app.connect(cfg, window, cx, move |res, window, cx| {
                    home.update(cx, |h, cx| {
                        h.connecting.remove(&retry.id);
                        let ok = res.is_ok();
                        match res {
                            Ok(()) => h.touch(&retry.id, window, cx),
                            Err(e) => {
                                h.failed.insert(retry.id.clone(), e);
                            }
                        }
                        // Imported connections have no password yet: let the user add it.
                        // (Not when the keychain failed: saving that form
                        // would replace the stored password with a blank.)
                        if !ok
                            && !keychain_failed
                            && retry.password.is_empty()
                            && retry.driver != Driver::Sqlite
                        {
                            h.open_form(Some(retry), window, cx);
                        }
                        cx.notify();
                    })
                    .ok();
                })
            })
            .ok();
    }

    fn import_from(
        &mut self,
        path: std::path::PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let bytes = match std::fs::read(&path) {
            Ok(b) => b,
            Err(e) => {
                window.push_notification(
                    Notification::error(format!("{e}")).title("Import failed"),
                    cx,
                );
                return;
            }
        };
        if import::needs_password(&bytes) {
            self.ask_import_password(bytes, window, cx);
        } else {
            self.finish_import(&bytes, None, window, cx);
        }
    }

    /// Returns false (keeping the password dialog open) on a wrong password.
    fn finish_import(
        &mut self,
        bytes: &[u8],
        password: Option<&str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let report = match import::parse(bytes, password, &self.connections) {
            Ok(r) => r,
            Err(e) => {
                window.push_notification(
                    Notification::error(format!("{e:#}")).title("Import failed"),
                    cx,
                );
                return false;
            }
        };
        let mut report = report;
        let mut keychain_err = None;
        for c in &report.added {
            let has_secret =
                !c.password.is_empty() || c.ssh.as_ref().is_some_and(|s| !s.secret.is_empty());
            if has_secret && let Err(e) = store::save_secrets(c) {
                report.keychain_failed += 1;
                keychain_err.get_or_insert(e);
            }
        }
        if let Some(e) = keychain_err {
            window.push_notification(
                Notification::warning(format!(
                    "{e:#}. Those connections will ask for their password when you open them."
                ))
                .title(format!(
                    "{} password(s) couldn't be saved",
                    report.keychain_failed
                )),
                cx,
            );
        }
        // Drop passwords from memory; they're read from the Keychain on connect.
        self.connections
            .extend(report.added.iter().cloned().map(|mut c| {
                c.password.clear();
                if let Some(ssh) = &mut c.ssh {
                    ssh.secret.clear();
                }
                c
            }));
        self.persist(window, cx);
        let note = if report.added.is_empty() {
            Notification::info(report.summary())
        } else {
            Notification::success(report.summary())
        };
        window.push_notification(note.title("TablePlus import"), cx);
        cx.notify();
        true
    }

    fn ask_import_password(&mut self, bytes: Vec<u8>, window: &mut Window, cx: &mut Context<Self>) {
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .masked(true)
                .placeholder("Export password")
        });
        let home = cx.entity().downgrade();
        let bytes = std::rc::Rc::new(bytes);
        input.update(cx, |i, cx| i.focus(window, cx));
        window.open_dialog(cx, move |dialog, _, _| {
            let (input2, home, bytes) = (input.clone(), home.clone(), bytes.clone());
            dialog
                .title("Password-protected export")
                .w(px(420.))
                .child(
                    v_flex()
                        .gap_2()
                        .child("Enter the password you chose when exporting from TablePlus.")
                        .child(Input::new(&input).mask_toggle()),
                )
                .footer(
                    DialogFooter::new()
                        .child(
                            Button::new("cancel-import")
                                .cursor_pointer()
                                .label("Cancel")
                                .small()
                                .on_click(|_, w, cx| w.close_dialog(cx)),
                        )
                        .child(
                            Button::new("do-import")
                                .cursor_pointer()
                                .label("Import")
                                .primary()
                                .small()
                                .on_click(move |_, window, cx| {
                                    let pw = input2.read(cx).value().to_string();
                                    let ok = home
                                        .update(cx, |h, cx| {
                                            h.finish_import(&bytes, Some(&pw), window, cx)
                                        })
                                        .unwrap_or(true);
                                    if ok {
                                        window.close_dialog(cx);
                                    }
                                }),
                        ),
                )
        });
    }

    fn import_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Import".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = rx.await else {
                return;
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };
            this.update_in(cx, |this, window, cx| this.import_from(path, window, cx))
                .ok();
        })
        .detach();
    }

    fn import_tableplus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let path = import::tableplus_store();
        if !path.exists() {
            window.push_notification(
                Notification::warning("TablePlus connections not found on this Mac"),
                cx,
            );
            return;
        }
        self.import_from(path, window, cx);
    }

    fn duplicate(&mut self, cfg: &ConnectionConfig, window: &mut Window, cx: &mut Context<Self>) {
        let mut copy = cfg.clone();
        if let Err(e) = store::load_secrets(&mut copy) {
            window.push_notification(
                Notification::error(format!("{e:#}")).title("Couldn't duplicate the connection"),
                cx,
            );
            return;
        }
        copy.id = Uuid::new_v4().to_string();
        copy.name = format!("{} copy", cfg.display_name());
        self.upsert(&copy, window, cx);
    }

    fn delete(&mut self, cfg: ConnectionConfig, window: &mut Window, cx: &mut Context<Self>) {
        let home = cx.entity().downgrade();
        let title = format!("Delete “{}”?", cfg.display_name());
        window.open_alert_dialog(cx, move |d, _, _| {
            let home = home.clone();
            let id = cfg.id.clone();
            d.title(title.clone())
                .description("The saved connection and its Keychain password will be removed.")
                .on_ok(move |_, window, cx| {
                    let id = id.clone();
                    home.update(cx, |h, cx| {
                        h.connections.retain(|c| c.id != id);
                        if let Err(e) = store::delete_password(&id) {
                            window.push_notification(
                                Notification::warning(format!("{e:#}")).title(
                                    "The connection was removed, but its Keychain item wasn't",
                                ),
                                cx,
                            );
                        }
                        h.persist(window, cx);
                        cx.notify();
                    })
                    .ok();
                    true
                })
        });
    }

    /// For screenshots.
    #[cfg(test)]
    pub fn set_failure(&mut self, id: &str, why: &str) {
        self.failed.insert(id.into(), why.into());
    }

    #[cfg(test)]
    pub fn failure(&self, id: &str) -> Option<String> {
        self.failed.get(id).cloned()
    }

    /// Marks `id` as just used, so the card can say when.
    fn touch(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(c) = self.connections.iter_mut().find(|c| c.id == id) {
            c.last_used = Some(Utc::now().timestamp());
            self.persist(window, cx);
        }
    }

    /// Shows the open connection's tab in the rail, or connects.
    fn open(&mut self, cfg: &ConnectionConfig, window: &mut Window, cx: &mut Context<Self>) {
        let open = self
            .app
            .upgrade()
            .map(|a| a.read(cx).open_ids(cx))
            .unwrap_or_default();
        match open.iter().position(|id| *id == cfg.id) {
            Some(i) => {
                self.app.update(cx, |a, cx| a.activate_session(i, cx)).ok();
            }
            None => self.connect(cfg.clone(), window, cx),
        }
    }

    fn render_card(
        &self,
        ix: usize,
        cfg: &ConnectionConfig,
        open_ix: Option<usize>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let t = tokens(cx);
        let connecting = self.connecting.contains(&cfg.id);
        let endpoint = match cfg.driver {
            Driver::Sqlite => cfg.file.clone(),
            _ if cfg.host.starts_with("mongodb") => "connection string".into(),
            _ => {
                let user = if cfg.user.is_empty() {
                    String::new()
                } else {
                    format!("{}@", cfg.user)
                };
                let db = if cfg.database.is_empty() {
                    String::new()
                } else {
                    format!("/{}", cfg.database)
                };
                format!("{user}{}:{}{db}", cfg.host, cfg.port)
            }
        };
        // Up to three facts worth a glance: tunnel, TLS, safe mode.
        let mut tags: Vec<(String, Hsla)> = vec![];
        if cfg.ssh.is_some() {
            tags.push(("SSH".into(), t.text_secondary));
        }
        if cfg.ssl {
            tags.push(("TLS".into(), t.text_secondary));
        }
        let (safe, safe_color, _) = safe_mode_badge(cfg, cx);
        if cfg.safety() != SafeMode::Off && cfg.safety() != SafeMode::Auto {
            tags.push((safe, safe_color));
        }
        tags.truncate(3);

        let failed = self.failed.get(&cfg.id).cloned();
        let footer: AnyElement = if let (Some(why), false) = (failed.clone(), connecting) {
            let (retry, edit) = (cfg.clone(), cfg.clone());
            let name = cfg.display_name();
            let home = cx.entity().downgrade();
            h_flex()
                .gap(dp(4.))
                .text_size(dp(11.5))
                .text_color(t.danger)
                .child(Icon::new(IconName::CircleX).xsmall())
                .child("Couldn't connect")
                .child(
                    Button::new(("retry", ix))
                        .cursor_pointer()
                        .label("Retry")
                        .ghost()
                        .xsmall()
                        .on_click(cx.listener(move |this, _, window, cx| {
                            cx.stop_propagation();
                            this.connect(retry.clone(), window, cx);
                        })),
                )
                .child(
                    Button::new(("why", ix))
                        .cursor_pointer()
                        .label("Details")
                        .ghost()
                        .xsmall()
                        .on_click(move |_, window, cx| {
                            cx.stop_propagation();
                            let (why, home, edit) = (why.clone(), home.clone(), edit.clone());
                            let title = format!("Couldn't connect to {name}");
                            window.open_alert_dialog(cx, move |d, _, _| {
                                let (home, edit) = (home.clone(), edit.clone());
                                d.title(title.clone())
                                    .description(why.clone())
                                    .show_cancel(true)
                                    .button_props(
                                        DialogButtonProps::default()
                                            .ok_text("Edit connection…")
                                            .cancel_text("Close"),
                                    )
                                    .on_ok(move |_, window, cx| {
                                        let (home, edit) = (home.clone(), edit.clone());
                                        // After this dialog has closed.
                                        window.defer(cx, move |window, cx| {
                                            home.update(cx, |h, cx| {
                                                h.open_form(Some(edit), window, cx)
                                            })
                                            .ok();
                                        });
                                        true
                                    })
                            });
                        }),
                )
                .into_any_element()
        } else if connecting {
            h_flex()
                .gap(dp(6.))
                .text_size(dp(11.5))
                .text_color(t.text_muted)
                .child(Spinner::new().xsmall())
                .child("Connecting…")
                .into_any_element()
        } else if let Some(i) = open_ix {
            h_flex()
                .gap(dp(6.))
                .text_size(dp(11.5))
                .text_color(t.success)
                .child(div().size(dp(6.)).rounded_full().bg(t.success))
                .child("Open")
                .children(shortcut_hint(i).map(|k| kbd(k, cx)))
                .into_any_element()
        } else {
            div()
                .text_size(dp(11.5))
                .text_color(t.text_faint)
                .child(match cfg.last_used {
                    Some(at) => format!("Used {}", ago(Utc::now().timestamp() - at)),
                    None => "Never opened".into(),
                })
                .into_any_element()
        };

        let (c1, c2, c3, c4) = (cfg.clone(), cfg.clone(), cfg.clone(), cfg.clone());
        let home = cx.entity().downgrade();
        let actions = h_flex()
            .gap(dp(2.))
            .invisible()
            .group_hover("conn-card", |s| s.visible())
            .child(
                Button::new(("edit", ix))
                    .cursor_pointer()
                    .icon(IconName::Pencil)
                    .ghost()
                    .xsmall()
                    .tooltip("Edit")
                    .on_click(cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        this.open_form(Some(c1.clone()), window, cx);
                    })),
            )
            .child(
                Button::new(("dup", ix))
                    .cursor_pointer()
                    .icon(IconName::Copy)
                    .ghost()
                    .xsmall()
                    .tooltip("Duplicate")
                    .on_click(cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        this.duplicate(&c2, window, cx);
                    })),
            )
            .child(
                Button::new(("del", ix))
                    .cursor_pointer()
                    .icon(IconName::Trash)
                    .ghost()
                    .xsmall()
                    .tooltip("Delete")
                    .on_click(cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        this.delete(c3.clone(), window, cx);
                    })),
            );

        v_flex()
            .id(("conn", ix))
            .debug_selector(move || format!("conn-card-{ix}"))
            .group("conn-card")
            .h(dp(128.))
            .min_w_0()
            .p(dp(14.))
            .gap(dp(6.))
            .rounded(dp(12.))
            .border_1()
            .border_color(if failed.is_some() && !connecting {
                t.danger.opacity(0.45)
            } else if open_ix.is_some() {
                t.success.opacity(0.35)
            } else {
                t.border
            })
            .bg(t.sidebar)
            .hover(|s| s.bg(t.hover).border_color(t.accent.opacity(0.5)))
            .cursor_pointer()
            .child(
                h_flex()
                    .gap(dp(12.))
                    .items_start()
                    .child(engine_badge(cfg.driver, 36., cx))
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap(dp(3.))
                            .child(
                                div()
                                    .text_size(dp(13.5))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(t.text)
                                    .truncate()
                                    .child(cfg.display_name()),
                            )
                            .child(
                                div()
                                    .font_family(cx.theme().mono_font_family.clone())
                                    .text_size(dp(11.5))
                                    .text_color(t.text_muted)
                                    .truncate()
                                    .child(endpoint),
                            ),
                    )
                    .child(actions),
            )
            .child(
                // Same height with or without tags, so footers line up.
                h_flex()
                    .h(dp(18.))
                    .gap(dp(6.))
                    .children(tags.into_iter().map(|(label, color)| {
                        div()
                            .flex()
                            .items_center()
                            .h(dp(18.))
                            .px(dp(6.))
                            .rounded(dp(5.))
                            .border_1()
                            .border_color(t.border)
                            .text_size(dp(10.5))
                            .text_color(color)
                            .child(label)
                    })),
            )
            .child(div().flex_1())
            .child(
                h_flex()
                    .child(env_pill(cfg.tag, cx))
                    .child(div().flex_1())
                    .child(footer),
            )
            .on_click({
                let cfg = cfg.clone();
                cx.listener(move |this, ev: &ClickEvent, window, cx| {
                    // An open connection switches on one click; opening a
                    // saved one takes a double click (or the menu).
                    if open_ix.is_some() || ev.click_count() == 2 {
                        this.open(&cfg, window, cx);
                    }
                })
            })
            .context_menu({
                let cfg = c4;
                move |menu, _, _| {
                    let cfg = cfg.clone();
                    let (h1, h2, h3, h4) = (home.clone(), home.clone(), home.clone(), home.clone());
                    let (a, b, c, d) = (cfg.clone(), cfg.clone(), cfg.clone(), cfg);
                    menu.item(
                        PopupMenuItem::new(if open_ix.is_some() { "Show" } else { "Connect" })
                            .on_click(move |_, w, cx| {
                                h1.update(cx, |h, cx| h.open(&a, w, cx)).ok();
                            }),
                    )
                    .item(PopupMenuItem::new("Edit…").on_click(move |_, w, cx| {
                        h2.update(cx, |h, cx| h.open_form(Some(b.clone()), w, cx))
                            .ok();
                    }))
                    .item(PopupMenuItem::new("Duplicate").on_click(move |_, w, cx| {
                        h3.update(cx, |h, cx| h.duplicate(&c, w, cx)).ok();
                    }))
                    .separator()
                    .item(PopupMenuItem::new("Delete…").on_click(move |_, w, cx| {
                        h4.update(cx, |h, cx| h.delete(d.clone(), w, cx)).ok();
                    }))
                }
            })
            .into_any_element()
    }

    /// Cards in rows of `cols`.
    fn grid(cards: Vec<AnyElement>, cols: usize) -> Div {
        div()
            .grid()
            .grid_cols(cols as u16)
            .gap(dp(16.))
            .children(cards)
    }

    fn import_button(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let (h1, h2) = (cx.entity().downgrade(), cx.entity().downgrade());
        Button::new("import")
            .cursor_pointer()
            .icon(IconName::Download)
            .label("Import")
            .outline()
            .dropdown_caret(true)
            .dropdown_menu(move |menu, _, _| {
                let (h1, h2) = (h1.clone(), h2.clone());
                menu.item(PopupMenuItem::new("From TablePlus on this Mac").on_click(
                    move |_, w, cx| {
                        h1.update(cx, |h, cx| h.import_tableplus(w, cx)).ok();
                    },
                ))
                .item(PopupMenuItem::new("From TablePlus export file…").on_click(
                    move |_, w, cx| {
                        h2.update(cx, |h, cx| h.import_file(w, cx)).ok();
                    },
                ))
            })
    }

    fn render_empty(&self, cx: &mut Context<Self>) -> AnyElement {
        let t = tokens(cx);
        v_flex()
            .items_center()
            .gap(dp(14.))
            .pt(dp(90.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_center()
                    .size(dp(56.))
                    .rounded(dp(14.))
                    .bg(t.accent.opacity(0.12))
                    .child(
                        Icon::new(IconName::DatabasePlus)
                            .size(dp(26.))
                            .text_color(t.accent),
                    ),
            )
            .child(
                div()
                    .text_size(dp(16.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(t.text)
                    .child("No connections yet"),
            )
            .child(
                div()
                    .text_size(dp(12.5))
                    .text_color(t.text_muted)
                    .child("Add one, or bring your connections over from TablePlus."),
            )
            .child(
                h_flex()
                    .gap(dp(8.))
                    .pt(dp(4.))
                    .child(
                        Button::new("first-conn")
                            .cursor_pointer()
                            .icon(IconName::Plus)
                            .label("New connection")
                            .primary()
                            .on_click(
                                cx.listener(|this, _, window, cx| this.open_form(None, window, cx)),
                            ),
                    )
                    .child(self.import_button(cx)),
            )
            .child(
                h_flex()
                    .gap(dp(6.))
                    .pt(dp(8.))
                    .text_size(dp(11.5))
                    .text_color(t.text_faint)
                    .child(Icon::new(IconName::Lock).xsmall())
                    .child(format!(
                        "Passwords are kept in {}, never in Tabula's files.",
                        keychain_name()
                    )),
            )
            .into_any_element()
    }
}

/// "just now", "5 min ago", "3 h ago", "yesterday", "4 days ago".
fn ago(secs: i64) -> String {
    match secs.max(0) {
        s if s < 60 => "just now".into(),
        s if s < 3600 => format!("{} min ago", s / 60),
        s if s < 86_400 => format!("{} h ago", s / 3600),
        s if s < 2 * 86_400 => "yesterday".into(),
        s if s < 30 * 86_400 => format!("{} days ago", s / 86_400),
        s if s < 365 * 86_400 => format!("{} months ago", (s / (30 * 86_400)).max(1)),
        _ => "over a year ago".into(),
    }
}

const ENV_FILTERS: [(Option<EnvTag>, &str); 5] = [
    (None, "All"),
    (Some(EnvTag::Local), "Local"),
    (Some(EnvTag::Development), "Dev"),
    (Some(EnvTag::Staging), "Staging"),
    (Some(EnvTag::Production), "Prod"),
];

impl Focusable for Home {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for Home {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = tokens(cx);
        // So `/` reaches the search box with nothing else focused.
        if window.focused(cx).is_none() {
            self.focus.focus(window, cx);
        }
        let q = self.search.read(cx).value().to_lowercase();
        let open = self
            .app
            .upgrade()
            .map(|a| a.read(cx).open_ids(cx))
            .unwrap_or_default();
        let visible: Vec<(usize, ConnectionConfig)> = self
            .connections
            .iter()
            .enumerate()
            .filter(|(_, c)| self.env_filter.is_none_or(|e| c.tag == e))
            .filter(|(_, c)| {
                q.is_empty()
                    || c.display_name().to_lowercase().contains(&q)
                    || c.host.to_lowercase().contains(&q)
                    || c.database.to_lowercase().contains(&q)
                    || c.driver.label().to_lowercase().contains(&q)
            })
            .map(|(i, c)| (i, c.clone()))
            .collect();
        // 380px cards, up to three across.
        let width = window.viewport_size().width / window.rem_size() * 13.;
        let cols = (((width - 60. - 64.) / 396.).floor() as usize).clamp(1, 3);

        let mut open_cards = vec![];
        let mut saved_cards = vec![];
        for (i, c) in &visible {
            let open_ix = open.iter().position(|id| *id == c.id);
            let card = self.render_card(*i, c, open_ix, cx);
            if open_ix.is_some() {
                open_cards.push((open_ix, card));
            } else {
                saved_cards.push(card);
            }
        }
        open_cards.sort_by_key(|(i, _)| *i);
        let open_cards: Vec<AnyElement> = open_cards.into_iter().map(|(_, c)| c).collect();
        let (n_open, n_saved) = (open_cards.len(), saved_cards.len());

        let env_ix = ENV_FILTERS
            .iter()
            .position(|(e, _)| *e == self.env_filter)
            .unwrap_or(0);
        let me = cx.entity();
        let toolbar = h_flex()
            .gap(dp(8.))
            .child(
                div().flex_1().child(
                    Input::new(&self.search)
                        .cleanable(true)
                        .prefix(Icon::new(IconName::Search).small().text_color(t.text_muted))
                        .suffix(kbd("/", cx)),
                ),
            )
            .child(
                dialog::segmented(
                    "env-filter",
                    &ENV_FILTERS.map(|(_, l)| l),
                    env_ix,
                    Rc::new(move |i, _, cx| {
                        me.update(cx, |h, cx| {
                            h.env_filter = ENV_FILTERS[i].0;
                            cx.notify();
                        })
                    }),
                    cx,
                )
                .w(dp(330.)),
            )
            .child(self.import_button(cx))
            .child(
                Button::new("new-conn")
                    .cursor_pointer()
                    .icon(IconName::Plus)
                    .label("New connection")
                    .primary()
                    .tooltip("⌘N")
                    .on_click(cx.listener(|this, _, window, cx| this.open_form(None, window, cx))),
            );

        let section = |label: &str, n: usize, cards: Vec<AnyElement>, cx: &App| {
            v_flex()
                .gap(dp(10.))
                .child(
                    h_flex().gap(dp(8.)).child(caps(label, cx)).child(
                        div()
                            .text_size(dp(11.))
                            .text_color(tokens(cx).text_faint)
                            .child(n.to_string()),
                    ),
                )
                .child(Self::grid(cards, cols))
        };

        let body: AnyElement = if self.connections.is_empty() {
            self.render_empty(cx)
        } else if visible.is_empty() {
            v_flex()
                .items_center()
                .gap(dp(10.))
                .pt(dp(60.))
                .text_size(dp(12.5))
                .text_color(t.text_muted)
                .child("No connection matches.")
                .child(
                    Button::new("clear-filters")
                        .cursor_pointer()
                        .label("Clear search and filter")
                        .outline()
                        .small()
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.env_filter = None;
                            this.search.update(cx, |s, cx| s.set_value("", window, cx));
                            cx.notify();
                        })),
                )
                .into_any_element()
        } else {
            v_flex()
                .gap(dp(24.))
                .when(n_open > 0, |d| {
                    d.child(section("Open now", n_open, open_cards, cx))
                })
                .when(n_saved > 0, |d| {
                    d.child(section("Saved", n_saved, saved_cards, cx))
                })
                .into_any_element()
        };

        let count = self.connections.len();
        div()
            .id("home-scroll")
            .track_focus(&self.focus)
            .key_context("Home")
            .on_key_down(cx.listener(|this, ev: &KeyDownEvent, window, cx| {
                if ev.keystroke.key == "/" && !ev.keystroke.modifiers.modified() {
                    cx.stop_propagation();
                    this.search.update(cx, |s, cx| s.focus(window, cx));
                }
            }))
            .size_full()
            .overflow_y_scroll()
            .bg(t.canvas)
            .child(
                v_flex()
                    .w_full()
                    .max_w(dp(380. * 3. + 16. * 2. + 64.))
                    .mx_auto()
                    .px(dp(32.))
                    .py(dp(32.))
                    .gap(dp(20.))
                    .child(
                        v_flex()
                            .gap(dp(4.))
                            .child(
                                div()
                                    .text_size(dp(22.))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(t.text)
                                    .child("Connections"),
                            )
                            .child(
                                div()
                                    .text_size(dp(12.5))
                                    .text_color(t.text_muted)
                                    .child(match count {
                                        0 => "Each connection you open gets a tile in the left rail.".to_string(),
                                        1 => "1 saved · open as many as you like; each gets a tile in the left rail.".to_string(),
                                        n => format!("{n} saved · open as many as you like; each gets a tile in the left rail."),
                                    }),
                            ),
                    )
                    .when(count > 0, |d| d.child(toolbar))
                    .when_some(self.load_warning.clone(), |d, w| {
                        d.child(
                            div()
                                .p(dp(12.))
                                .rounded(dp(8.))
                                .text_size(dp(12.5))
                                .bg(t.warning.opacity(0.14))
                                .text_color(t.warning)
                                .child(w),
                        )
                    })
                    .child(body),
            )
    }
}
