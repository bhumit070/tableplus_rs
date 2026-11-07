//! Start screen: the saved-connection list.

use gpui::{
    Context, Entity, InteractiveElement, IntoElement, ParentElement, Render, Styled, Subscription,
    WeakEntity, Window, div, prelude::*, px,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::dialog::DialogFooter;
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::menu::{ContextMenuExt as _, DropdownMenu as _, PopupMenuItem};
use gpui_component::notification::Notification;
use gpui_component::{ActiveTheme as _, Icon, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit_assets::IconName;

use super::app::AppView;
use super::conn_form::ConnForm;
use super::tag_color;
use crate::db::{ConnectionConfig, Driver};
use crate::{import, store};

pub struct Home {
    app: WeakEntity<AppView>,
    pub connections: Vec<ConnectionConfig>,
    search: Entity<InputState>,
    /// Ids of connections being opened (several at launch).
    pub connecting: std::collections::HashSet<String>,
    /// Set when connections.json couldn't be read (it's kept aside).
    load_warning: Option<String>,
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
        let mut cfg = cfg.unwrap_or_else(|| ConnectionConfig::new(Driver::Postgres));
        // The form must show the real secret: a blank field saved back
        // would delete the stored password.
        if !is_new && let Err(e) = store::load_secrets(&mut cfg) {
            window.push_notification(
                Notification::error(format!("{e:#}")).title("Can't edit this connection right now"),
                cx,
            );
            return;
        }
        let form = cx.new(|cx| ConnForm::new(cfg, window, cx));
        let home = cx.entity().downgrade();
        window.open_dialog(cx, move |dialog, _, _| {
            let (f1, f2) = (form.clone(), form.clone());
            let (h1, h2) = (home.clone(), home.clone());
            dialog
                .title(if is_new {
                    "New Connection"
                } else {
                    "Edit Connection"
                })
                .w(px(560.))
                .child(form.clone())
                .footer(
                    DialogFooter::new()
                        .child(
                            Button::new("cancel")
                                .cursor_pointer()
                                .label("Cancel")
                                .small()
                                .on_click(|_, w, cx| w.close_dialog(cx)),
                        )
                        .child(
                            Button::new("save")
                                .cursor_pointer()
                                .label("Save")
                                .small()
                                .on_click(move |_, window, cx| {
                                    let cfg = f1.read(cx).config(cx);
                                    h1.update(cx, |h, cx| h.upsert(&cfg, window, cx)).ok();
                                    window.close_dialog(cx);
                                }),
                        )
                        .child(
                            Button::new("connect")
                                .cursor_pointer()
                                .label("Connect")
                                .primary()
                                .small()
                                .on_click(move |_, window, cx| {
                                    let cfg = f2.read(cx).config(cx);
                                    h2.update(cx, |h, cx| {
                                        h.upsert(&cfg, window, cx);
                                        h.connect(cfg.clone(), window, cx);
                                    })
                                    .ok();
                                    window.close_dialog(cx);
                                }),
                        ),
                )
        });
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
        cx.notify();
        let home = cx.entity().downgrade();
        self.app
            .update(cx, |app, cx| {
                let retry = cfg.clone();
                app.connect(cfg, window, cx, move |ok, window, cx| {
                    home.update(cx, |h, cx| {
                        h.connecting.remove(&retry.id);
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
        let rx = cx.prompt_for_paths(gpui::PathPromptOptions {
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
        copy.id = uuid::Uuid::new_v4().to_string();
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

    fn render_row(
        &self,
        ix: usize,
        cfg: &ConnectionConfig,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = cx.theme();
        let connecting = self.connecting.contains(&cfg.id);
        let subtitle = match cfg.driver {
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
                    format!(" / {}", cfg.database)
                };
                format!("{user}{}:{}{db}", cfg.host, cfg.port)
            }
        };
        let (c1, c2, c3, c4) = (cfg.clone(), cfg.clone(), cfg.clone(), cfg.clone());
        let home = cx.entity().downgrade();
        div()
            .id(("conn", ix))
            .group("conn-row")
            .flex()
            .items_center()
            .gap_3()
            .px_3()
            .py_2()
            .rounded_lg()
            .border_1()
            .border_color(theme.border)
            .bg(theme.background)
            .hover(|s| s.bg(theme.list_hover))
            .cursor_pointer()
            .child(
                div()
                    .w(px(4.))
                    .h(px(34.))
                    .rounded_full()
                    .bg(tag_color(cfg, cx)),
            )
            .child(
                div()
                    .size(px(34.))
                    .rounded_md()
                    .flex()
                    .items_center()
                    .justify_center()
                    .bg(theme.secondary)
                    .text_xs()
                    .font_weight(gpui::FontWeight::BOLD)
                    .child(cfg.driver.short()),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(
                        div()
                            .text_sm()
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .truncate()
                            .child(cfg.display_name()),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .truncate()
                            .child(subtitle),
                    ),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(tag_color(cfg, cx))
                    .child(cfg.tag.label()),
            )
            .child(
                h_flex()
                    .gap_0p5()
                    .invisible()
                    .group_hover("conn-row", |s| s.visible())
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
                    ),
            )
            .child(
                Button::new(("open", ix))
                    .cursor_pointer()
                    .label("Connect")
                    .small()
                    .primary()
                    .loading(connecting)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        this.connect(c4.clone(), window, cx);
                    })),
            )
            .on_click({
                let cfg = cfg.clone();
                cx.listener(move |this, ev: &gpui::ClickEvent, window, cx| {
                    if ev.click_count() == 2 {
                        this.connect(cfg.clone(), window, cx);
                    }
                })
            })
            .context_menu({
                let cfg = cfg.clone();
                move |menu, _, _| {
                    let cfg = cfg.clone();
                    let (h1, h2, h3, h4) = (home.clone(), home.clone(), home.clone(), home.clone());
                    let (a, b, c, d) = (cfg.clone(), cfg.clone(), cfg.clone(), cfg);
                    menu.item(PopupMenuItem::new("Connect").on_click(move |_, w, cx| {
                        h1.update(cx, |h, cx| h.connect(a.clone(), w, cx)).ok();
                    }))
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
    }
}

impl Render for Home {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let q = self.search.read(cx).value().to_lowercase();
        let visible: Vec<(usize, ConnectionConfig)> = self
            .connections
            .iter()
            .enumerate()
            .filter(|(_, c)| {
                q.is_empty()
                    || c.display_name().to_lowercase().contains(&q)
                    || c.host.to_lowercase().contains(&q)
                    || c.database.to_lowercase().contains(&q)
            })
            .map(|(i, c)| (i, c.clone()))
            .collect();
        let list = if self.connections.is_empty() {
            v_flex()
                .items_center()
                .gap_3()
                .py_10()
                .text_color(theme.muted_foreground)
                .child(Icon::new(IconName::DatabasePlus).size(px(40.)))
                .child("No connections yet")
                .child(
                    Button::new("first-conn")
                        .cursor_pointer()
                        .label("Create a connection")
                        .primary()
                        .on_click(
                            cx.listener(|this, _, window, cx| this.open_form(None, window, cx)),
                        ),
                )
                .into_any_element()
        } else {
            v_flex()
                .gap_2()
                .children(
                    visible
                        .iter()
                        .map(|(i, c)| self.render_row(*i, c, cx).into_any_element()),
                )
                .into_any_element()
        };
        div().id("home-scroll").size_full().overflow_y_scroll().bg(theme.background).child(
            v_flex()
                .w_full()
                .max_w(px(760.))
                .mx_auto()
                .px_6()
                .py_8()
                .gap_4()
                .child(
                    h_flex()
                        .gap_3()
                        .child(Icon::new(IconName::Database).size(px(28.)).text_color(theme.primary))
                        .child(
                            v_flex()
                                .child(div().text_xl().font_weight(gpui::FontWeight::BOLD).child("Connections"))
                                .child(
                                    div()
                                        .text_sm()
                                        .text_color(theme.muted_foreground)
                                        .child("Open as many as you like — each one appears in the left rail."),
                                ),
                        ),
                )
                .child(
                    h_flex()
                        .gap_2()
                        .child(div().flex_1().child(Input::new(&self.search).cleanable(true).prefix(Icon::new(IconName::Search).small())))
                        .child({
                            let (h1, h2) = (cx.entity().downgrade(), cx.entity().downgrade());
                            Button::new("import").cursor_pointer()
                                .icon(IconName::Download)
                                .label("Import")
                                .dropdown_caret(true)
                                .dropdown_menu(move |menu, _, _| {
                                    let (h1, h2) = (h1.clone(), h2.clone());
                                    menu.item(PopupMenuItem::new("From TablePlus on this Mac").on_click(move |_, w, cx| {
                                        h1.update(cx, |h, cx| h.import_tableplus(w, cx)).ok();
                                    }))
                                    .item(PopupMenuItem::new("From TablePlus export file…").on_click(move |_, w, cx| {
                                        h2.update(cx, |h, cx| h.import_file(w, cx)).ok();
                                    }))
                                })
                        })
                        .child(
                            Button::new("new-conn").cursor_pointer()
                                .icon(IconName::Plus)
                                .label("New Connection")
                                .primary()
                                .tooltip("⌘N")
                                .on_click(cx.listener(|this, _, window, cx| this.open_form(None, window, cx))),
                        ),
                )
                .when_some(self.load_warning.clone(), |d, w| {
                    d.child(
                        div()
                            .p_3()
                            .rounded_md()
                            .text_sm()
                            .bg(theme.warning.opacity(0.15))
                            .text_color(theme.warning)
                            .child(w),
                    )
                })
                .child(list),
        )
    }
}
