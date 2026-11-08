//! Window root: title bar, the connection rail on the left (home + one icon
//! per open connection, no limit), and the active view.

use std::cell::RefCell;
use std::cmp::Reverse;
use std::rc::Rc;
use std::sync::Arc;

use gpui::{
    AnyElement, AnyWindowHandle, App, Context, Entity, FocusHandle, FontWeight, Global,
    InteractiveElement, IntoElement, ParentElement, Render, SharedString, Styled, WeakEntity,
    Window, div, prelude::*, px,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::notification::Notification;
use gpui_component::spinner::Spinner;
use gpui_component::tooltip::Tooltip;
use gpui_component::{
    ActiveTheme as _, Icon, Sizable as _, Theme, ThemeMode, TitleBar, WindowExt as _, h_flex,
    v_flex,
};
use gpui_kit_assets::IconName;
use tokio::sync::RwLock;

use super::home::Home;
use super::onboarding::Onboarding;
use super::picker::{OnPick, PickItem, Picker};
use super::session::Session;
use super::settings_view::SettingsView;
use super::table_tab::TableTab;
use super::theme::{dp, env_pill, kbd, keychain_name, safe_mode_badge, tokens};
use super::{
    AddRow, BackupDatabase, CancelQuery, CloseTab, Commit, Connection1, Connection2, Connection3,
    Connection4, Connection5, Connection6, Connection7, Connection8, Connection9, DeleteRows,
    Discard, DropDatabase, ExplainQuery, ExportData, FormatSql, GoHome, GoTo, NewDatabase,
    NewQuery, NextTab, OpenConnection, OpenSettings, PrevTab, QueryHistory, Refresh,
    RestoreDatabase, RunAll, RunQuery, SaveQuery, SavedQueries, Shared, ShowProcesses,
    SwitchDatabase, ToggleFilter, ToggleRowDetail, ToggleTheme, tag_color,
};
use crate::db::ssh::UnknownHostKey;
use crate::db::{Connection, ConnectionConfig};
use crate::runtime;
use crate::settings;

pub struct AppView {
    home: Entity<Home>,
    sessions: Vec<Entity<Session>>,
    /// `None` shows the connection list.
    active: Option<usize>,
    focus: FocusHandle,
    /// Saved tabs of connections reopening at launch, by connection id
    /// (and whether that session was in front).
    pending_restore: std::collections::HashMap<String, (crate::restore::SessionState, bool)>,
    /// First-run onboarding (design.md 24), shown in place of everything
    /// else until finished or skipped.
    onboarding: Option<Entity<Onboarding>>,
}

impl AppView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let me = cx.entity().downgrade();
        let home = cx.new(|cx| Home::new(me, window, cx));
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        cx.set_global(AppHandle {
            view: cx.entity().downgrade(),
            window: window.window_handle(),
        });
        // Last time's state is saved on quit, and reopened once at launch.
        cx.on_app_quit(|this: &mut AppView, cx| {
            this.save_state(cx);
            async {}
        })
        .detach();
        // A fresh install sees onboarding; an existing one (it already has
        // connections) counts as onboarded and never does.
        let has_connections = !home.read(cx).connections.is_empty();
        let mut onboarding = None;
        if !settings::get(cx).onboarded && !cfg!(test) {
            if has_connections {
                settings::update(cx, |s| s.onboarded = true);
            } else {
                let me = cx.entity().downgrade();
                let home = home.clone();
                onboarding = Some(cx.new(|cx| Onboarding::new(me, home, cx)));
            }
        }
        if settings::get(cx).restore_sessions && onboarding.is_none() && !cfg!(test) {
            cx.defer_in(window, |this, window, cx| this.offer_restore(window, cx));
        }
        Self {
            home,
            sessions: Vec::new(),
            active: None,
            focus,
            pending_restore: Default::default(),
            onboarding,
        }
    }

    fn snapshot(&self, cx: &App) -> crate::restore::State {
        // Last session, while it's still on offer (24.6): quitting without
        // answering mustn't lose it.
        let offered = self.home.read(cx).reopen_offer().cloned();
        crate::restore::State {
            // Ones still reconnecting (or that failed to) keep their tabs too.
            sessions: self
                .sessions
                .iter()
                .map(|s| s.read(cx).snapshot(cx))
                .chain(self.pending_restore.values().map(|(s, _)| s.clone()))
                .chain(offered.iter().flat_map(|o| o.sessions.clone()))
                .collect(),
            active: self
                .active
                .and_then(|i| self.sessions.get(i))
                .map(|s| s.read(cx).shared.cfg.id.clone()),
        }
    }

    /// Best effort: losing the session list isn't worth interrupting for,
    /// but it's logged.
    pub fn save_state(&self, cx: &App) {
        if let Err(e) = crate::restore::save(&self.snapshot(cx)) {
            eprintln!("couldn't save the session: {e:#}");
        }
    }

    /// At launch: last session goes on offer on the home screen ("Pick up
    /// where you left off?", design.md 24.6), so nothing connects or reads
    /// the keychain until asked; or reopens at once with "always reopen".
    fn offer_restore(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (state, warning) = crate::restore::load();
        if let Some(w) = warning {
            window.push_notification(Notification::warning(w), cx);
        }
        let saved = self.home.read(cx).connections.clone();
        let mut state = state;
        state
            .sessions
            .retain(|s| saved.iter().any(|c| c.id == s.conn_id));
        if state.sessions.is_empty() {
            return;
        }
        if crate::settings::get(cx).always_reopen {
            self.restore_from(state, window, cx);
        } else {
            self.home.update(cx, |h, cx| h.offer_reopen(state, cx));
        }
    }

    /// Connects `state`'s saved connections; their tabs reopen as each one
    /// connects (see `connect`).
    pub fn restore_from(
        &mut self,
        state: crate::restore::State,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let saved = self.home.read(cx).connections.clone();
        for s in state.sessions {
            let Some(cfg) = saved.iter().find(|c| c.id == s.conn_id).cloned() else {
                continue;
            };
            let front = state.active.as_deref() == Some(s.conn_id.as_str());
            self.pending_restore.insert(s.conn_id.clone(), (s, front));
            // Home::connect updates this view: run it after this update.
            let home = self.home.clone();
            window.defer(cx, move |window, cx| {
                home.update(cx, |h, cx| h.connect(cfg, window, cx));
            });
        }
    }

    /// Opens a connection in a new session. `done` runs either way, with
    /// the error when it failed.
    pub fn connect(
        &mut self,
        cfg: ConnectionConfig,
        window: &mut Window,
        cx: &mut Context<Self>,
        done: impl FnOnce(Connected, &mut Window, &mut App) + 'static,
    ) {
        let c = cfg.clone();
        let task = runtime::spawn(async move { Connection::connect(&c).await });
        cx.spawn_in(window, async move |this, cx| {
            let res = task.await;
            this.update_in(cx, |this, window, cx| {
                // A new SSH host: ask before trusting its key, then retry.
                if let Err(e) = &res
                    && let Some(unknown) = e.downcast_ref::<UnknownHostKey>()
                {
                    this.confirm_host_key(unknown.clone(), cfg, window, cx, done);
                    return;
                }
                done(
                    res.as_ref().map(|_| ()).map_err(|e| format!("{e:#}")),
                    window,
                    cx,
                );
                match res {
                    Ok(conn) => {
                        let shared = Rc::new(Shared {
                            lost: Default::default(),
                            cfg,
                            conn: Arc::new(RwLock::new(conn)),
                            schema: Default::default(),
                        });
                        let id = shared.cfg.id.clone();
                        let session = cx.new(|cx| Session::new(shared, window, cx));
                        this.sessions.push(session.clone());
                        let me = this.sessions.len() - 1;
                        match this.pending_restore.remove(&id) {
                            Some((state, front)) => {
                                session.update(cx, |s, cx| s.restore_tabs(state, window, cx));
                                if front || this.active.is_none() {
                                    this.active = Some(me);
                                }
                            }
                            None => this.active = Some(me),
                        }
                        this.save_state(cx);
                    }
                    Err(e) => window.push_notification(
                        Notification::error(format!("{e:#}"))
                            .title(format!("Couldn't connect to {}", cfg.display_name())),
                        cx,
                    ),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn confirm_host_key(
        &mut self,
        unknown: UnknownHostKey,
        cfg: ConnectionConfig,
        window: &mut Window,
        cx: &mut Context<Self>,
        done: impl FnOnce(Connected, &mut Window, &mut App) + 'static,
    ) {
        type Done = Box<dyn FnOnce(Connected, &mut Window, &mut App)>;
        let done: Rc<RefCell<Option<Done>>> = Rc::new(RefCell::new(Some(Box::new(done))));
        let me = cx.entity().downgrade();
        let body: SharedString = format!(
            "{}\n\nTrust it only if this fingerprint is the server's. Tabula will remember it.",
            unknown
        )
        .into();
        window.open_alert_dialog(cx, move |d, _, _| {
            let (me, unknown, cfg) = (me.clone(), unknown.clone(), cfg.clone());
            let (done_ok, done_cancel) = (done.clone(), done.clone());
            d.title("Trust this SSH host?")
                .description(body.clone())
                .show_cancel(true)
                .on_ok(move |_, window, cx| {
                    let Some(done) = done_ok.borrow_mut().take() else {
                        return true;
                    };
                    if let Err(e) = unknown.trust() {
                        let why = format!("Couldn't save the host key: {e:#}");
                        window.push_notification(Notification::error(why.clone()), cx);
                        done(Err(why), window, cx);
                        return true;
                    }
                    let cfg = cfg.clone();
                    me.update(cx, |a, cx| a.connect(cfg, window, cx, done)).ok();
                    true
                })
                .on_cancel(move |_, window, cx| {
                    if let Some(done) = done_cancel.borrow_mut().take() {
                        done(Err("The SSH host key wasn't trusted.".into()), window, cx);
                    }
                    true
                })
        });
    }

    fn close_session(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(s) = self.sessions.get(ix) else {
            return;
        };
        if s.read(cx).has_unsaved(cx) {
            let me = cx.entity().downgrade();
            window.open_alert_dialog(cx, move |d, _, _| {
                let me = me.clone();
                d.title("Close connection?")
                    .description("Some tabs have uncommitted changes.")
                    .on_ok(move |_, _, cx| {
                        me.update(cx, |a, cx| a.remove_session(ix, cx)).ok();
                        true
                    })
            });
            return;
        }
        self.remove_session(ix, cx);
    }

    fn remove_session(&mut self, ix: usize, cx: &mut Context<Self>) {
        if ix >= self.sessions.len() {
            return;
        }
        let s = self.sessions.remove(ix);
        let conn = s.read(cx).shared.conn.clone();
        // Close pools in the background.
        runtime::spawn_detached(async move { conn.read().await.close().await });
        self.active = match self.active {
            Some(a) if a == ix => {
                (!self.sessions.is_empty()).then(|| ix.min(self.sessions.len() - 1))
            }
            Some(a) if a > ix => Some(a - 1),
            other => other,
        };
        self.save_state(cx);
        cx.notify();
    }

    #[cfg(test)]
    pub fn reopen_for_test(
        &mut self,
        state: crate::restore::SessionState,
        cfg: ConnectionConfig,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.pending_restore
            .insert(state.conn_id.clone(), (state, true));
        self.connect(cfg, window, cx, |_, _, _| {});
    }

    #[cfg(test)]
    pub fn snapshot_for_test(&self, cx: &App) -> crate::restore::State {
        self.snapshot(cx)
    }

    pub fn active_session(&self) -> Option<Entity<Session>> {
        self.active.and_then(|i| self.sessions.get(i)).cloned()
    }

    /// A connection tile on the rail (design.md, Components: Connection
    /// tile), or the home tile with `None`.
    /// A tile per connection still opening: so opening one from ⌘N (with
    /// the home screen hidden) shows something is happening.
    fn connecting_tiles(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let t = tokens(cx);
        let home = self.home.read(cx);
        let open = self.open_ids(cx);
        home.connections
            .iter()
            .filter(|c| home.connecting.contains(&c.id) && !open.contains(&c.id))
            .enumerate()
            .map(|(i, cfg)| {
                let tip = format!("Connecting to {}…", cfg.display_name());
                div()
                    .id(("rail-connecting", i))
                    .debug_selector(move || format!("rail-connecting-{i}"))
                    .size(dp(36.))
                    .rounded(dp(10.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .border_1()
                    .border_color(tag_color(cfg, cx).opacity(0.5))
                    .bg(t.surface)
                    .child(Spinner::new().small().color(tag_color(cfg, cx)))
                    .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
                    .into_any_element()
            })
            .collect()
    }

    fn rail_button(&self, ix: Option<usize>, cx: &mut Context<Self>) -> impl IntoElement {
        let t = tokens(cx);
        let selected = self.active == ix;
        let (label, color, tip) = match ix {
            None => (None, t.text_secondary, "Connections (⌘0)".to_string()),
            Some(i) => {
                let s = self.sessions[i].read(cx);
                let cfg = &s.shared.cfg;
                let initials: String = cfg
                    .display_name()
                    .chars()
                    .filter(|c| c.is_alphanumeric())
                    .take(2)
                    .collect();
                (
                    Some(initials.to_uppercase()),
                    tag_color(cfg, cx),
                    match shortcut_hint(i) {
                        Some(k) => format!("{} · {} ({k})", cfg.display_name(), cfg.driver.label()),
                        None => format!("{} · {}", cfg.display_name(), cfg.driver.label()),
                    },
                )
            }
        };
        let id = ix.map_or(usize::MAX, |i| i);
        let tile = div()
            .id(("rail", id))
            .group("rail-item")
            .relative()
            .size(dp(36.))
            .rounded(dp(10.))
            .flex()
            .items_center()
            .justify_center()
            .cursor_pointer()
            .text_size(dp(12.))
            .font_weight(FontWeight::SEMIBOLD)
            .map(|d| match &label {
                // Env colour mixed into the surface, initials in the env colour.
                Some(l) => d
                    .bg(t.surface.blend(color.opacity(0.20)))
                    .text_color(color.blend(t.text.opacity(0.35)))
                    .hover(|d| d.bg(t.surface.blend(color.opacity(0.30))))
                    .child(l.clone())
                    .child(
                        div()
                            .absolute()
                            .bottom(dp(-2.))
                            .right(dp(-2.))
                            .size(dp(10.))
                            .rounded_full()
                            .border_2()
                            .border_color(t.rail)
                            .bg(color),
                    ),
                None => d
                    .text_color(if selected { t.text } else { t.text_secondary })
                    .when(selected, |d| d.bg(t.hover))
                    .hover(|d| d.bg(t.hover))
                    .child(Icon::new(IconName::LayoutGrid).small()),
            })
            .when(selected && label.is_some(), |d| {
                d.border_2().border_color(t.accent)
            })
            .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.active = ix;
                cx.notify();
            }))
            .when_some(ix, |d, i| {
                d.child(
                    div()
                        .id(("rail-close", i))
                        .absolute()
                        .top(dp(-5.))
                        .right(dp(-5.))
                        .size(dp(16.))
                        .rounded_full()
                        .flex()
                        .items_center()
                        .justify_center()
                        .bg(t.raised)
                        .border_1()
                        .border_color(t.border)
                        .text_color(t.text_secondary)
                        .invisible()
                        .group_hover("rail-item", |s| s.visible())
                        .cursor_pointer()
                        .child(Icon::new(IconName::X).xsmall())
                        .on_click(cx.listener(move |this, _, window, cx| {
                            cx.stop_propagation();
                            this.close_session(i, window, cx);
                        })),
                )
            });
        // The active tile also gets a bar on the rail's left edge.
        h_flex()
            .w_full()
            .h(dp(40.))
            .justify_center()
            .relative()
            .when(selected, |d| {
                d.child(
                    div()
                        .absolute()
                        .left_0()
                        .top(dp(10.))
                        .w(dp(3.))
                        .h(dp(20.))
                        .rounded_r(dp(2.))
                        .bg(t.accent),
                )
            })
            .child(tile)
    }

    fn render_rail(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let t = tokens(cx);
        v_flex()
            .w(dp(60.))
            .h_full()
            .flex_none()
            .items_center()
            .gap(dp(4.))
            .py(dp(10.))
            .bg(t.rail)
            .border_r_1()
            .border_color(t.border)
            .child(self.rail_button(None, cx))
            .child(div().w(dp(24.)).h(px(1.)).my(dp(4.)).bg(t.border))
            .children(
                (0..self.sessions.len()).map(|i| self.rail_button(Some(i), cx).into_any_element()),
            )
            .children(self.connecting_tiles(cx))
            .child(
                div()
                    .id("rail-new")
                    .mt(dp(2.))
                    .size(dp(36.))
                    .rounded(dp(10.))
                    .border_1()
                    .border_dashed()
                    .border_color(t.border)
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .text_color(t.text_muted)
                    .hover(|d| d.bg(t.hover).text_color(t.text))
                    .child(Icon::new(IconName::Plus).small())
                    .tooltip(|window, cx| Tooltip::new("Open connection (⌘N)").build(window, cx))
                    .on_click(
                        cx.listener(|this, _, window, cx| this.open_connection_picker(window, cx)),
                    ),
            )
            .child(div().flex_1())
            .child(
                Button::new("settings")
                    .cursor_pointer()
                    .icon(IconName::Settings)
                    .ghost()
                    .tooltip("Settings (⌘,)")
                    .on_click(|_, window, cx| {
                        SettingsView::open(window, cx);
                    }),
            )
    }

    /// Title bar: where you are (env, connection / database / schema), the
    /// go-to box, and safe mode (design.md, screen 01).
    fn render_title_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let t = tokens(cx);
        let dark = cx.theme().mode.is_dark();
        let session = self.active_session();
        let crumb = |id: &'static str, text: String| {
            h_flex()
                .id(id)
                .h(dp(26.))
                .px(dp(6.))
                .gap(dp(4.))
                .rounded(dp(6.))
                .cursor_pointer()
                .text_size(dp(13.))
                .text_color(t.text_secondary)
                .hover(|d| d.bg(t.hover).text_color(t.text))
                .child(text)
                .child(
                    Icon::new(IconName::ChevronDown)
                        .xsmall()
                        .text_color(t.text_muted),
                )
        };
        let slash = || div().text_color(t.text_faint).child("/");
        let left = match &session {
            None => h_flex().gap(dp(8.)).child(
                div()
                    .text_size(dp(13.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("Tabula"),
            ),
            Some(s) => {
                let sr = s.read(cx);
                let cfg = sr.cfg();
                let (s_db, s_schema) = (s.downgrade(), s.downgrade());
                h_flex()
                    .gap(dp(6.))
                    .min_w_0()
                    .child(env_pill(cfg.tag, cx))
                    .child(
                        div()
                            .text_size(dp(13.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .truncate()
                            .child(cfg.display_name()),
                    )
                    .when_some(sr.switching_to(), |d, to| {
                        d.child(slash()).child(
                            h_flex()
                                .gap(dp(6.))
                                .px(dp(6.))
                                .text_size(dp(13.))
                                .text_color(t.text_muted)
                                .child(Spinner::new().xsmall())
                                .child(format!("Switching to {to}…")),
                        )
                    })
                    .when(
                        !sr.current_db().is_empty() && sr.switching_to().is_none(),
                        |d| {
                            d.child(slash()).child(
                                crumb("crumb-db", sr.current_db().to_string())
                                    .tooltip(|window, cx| {
                                        Tooltip::new("Switch database (⌘K)").build(window, cx)
                                    })
                                    .on_click(move |_, window, cx| {
                                        s_db.update(cx, |s, cx| s.open_db_picker(window, cx)).ok();
                                    }),
                            )
                        },
                    )
                    .when_some(sr.schema_choice(), |d, schema| {
                        d.child(slash()).child(
                            crumb("crumb-schema", schema.unwrap_or("all schemas").to_string())
                                .tooltip(|window, cx| {
                                    Tooltip::new("Schema for unqualified names").build(window, cx)
                                })
                                .on_click(move |_, window, cx| {
                                    s_schema
                                        .update(cx, |s, cx| s.open_schema_picker(window, cx))
                                        .ok();
                                }),
                        )
                    })
            }
        };
        let goto = h_flex()
            .id("goto")
            .w(dp(360.))
            .h(dp(28.))
            .px(dp(10.))
            .gap(dp(8.))
            .rounded(dp(7.))
            .bg(t.surface)
            .border_1()
            .border_color(t.border)
            .cursor_pointer()
            .text_size(dp(12.))
            .text_color(t.text_muted)
            .hover(|d| d.border_color(t.text_faint))
            .child(Icon::new(IconName::Search).small())
            .child(div().flex_1().truncate().child(if session.is_some() {
                "Go to table, query, command…"
            } else {
                "Go to connection…"
            }))
            .child(kbd("⌘P", cx))
            .on_click(|_, window, cx| window.dispatch_action(Box::new(GoTo), cx));
        let right = h_flex()
            .gap(dp(8.))
            .justify_end()
            .when_some(session.as_ref(), |d, s| {
                let (label, color, icon) = safe_mode_badge(s.read(cx).cfg(), cx);
                d.child(
                    h_flex()
                        .id("safe-mode")
                        .h(dp(26.))
                        .px(dp(10.))
                        .gap(dp(6.))
                        .rounded(dp(7.))
                        .bg(color.opacity(0.12))
                        .text_size(dp(12.))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(color)
                        .map(|d| match icon {
                            Some(i) => d.child(Icon::new(i).xsmall()),
                            None => d.child(div().size(dp(6.)).rounded_full().bg(color)),
                        })
                        .child(label)
                        .tooltip(|window, cx| {
                            Tooltip::new("Safe mode: set in the connection's settings")
                                .build(window, cx)
                        }),
                )
                .child(
                    Button::new("new-query")
                        .cursor_pointer()
                        .icon(IconName::Plus)
                        .label("New Query")
                        .primary()
                        .small()
                        .tooltip("New query (⌘T)")
                        .on_click(|_, window, cx| window.dispatch_action(Box::new(NewQuery), cx)),
                )
            })
            .child(
                Button::new("theme")
                    .cursor_pointer()
                    .icon(if dark { IconName::Sun } else { IconName::Moon })
                    .outline()
                    .small()
                    .tooltip("Toggle theme (⌘⇧L)")
                    .on_click(cx.listener(|_, _, window, cx| toggle_theme(window, cx))),
            );
        TitleBar::new()
            .h(px(44.))
            .bg(t.rail)
            .border_color(t.border)
            .child(
                h_flex()
                    .w_full()
                    .h_full()
                    .pr(dp(12.))
                    .gap(dp(12.))
                    .child(div().flex_1().min_w_0().child(left))
                    .child(goto)
                    .child(div().flex_1().flex().justify_end().child(right)),
            )
    }

    /// Status bar: connection and server on the left, what the active tab
    /// shows on the right.
    fn render_status_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let t = tokens(cx);
        let sep = || div().text_color(t.text_faint).child("·");
        let bar = h_flex()
            .h(dp(26.))
            .flex_none()
            .px(dp(12.))
            .gap(dp(6.))
            .bg(t.rail)
            .border_t_1()
            .border_color(t.border)
            .text_size(dp(11.5))
            .text_color(t.text_muted);
        match self.active_session() {
            None => {
                let saved = self.home.read(cx).connections.len();
                bar.child(format!("{} open", self.sessions.len()))
                    .child(sep())
                    .child(format!(
                        "{saved} saved connection{}",
                        if saved == 1 { "" } else { "s" }
                    ))
                    .child(sep())
                    .child(format!("passwords stored in {}", keychain_name()))
            }
            Some(s) => {
                let s = s.read(cx);
                let cfg = s.cfg();
                let server = match s.server_version() {
                    Some(v) => format!("{} {v}", cfg.driver.label()),
                    None => cfg.driver.label().to_string(),
                };
                let lost = s.shared.lost.borrow().is_some();
                let (dot, state) = if lost {
                    (t.warning, "Reconnecting…")
                } else {
                    (t.success, "Connected")
                };
                bar.child(div().size(dp(7.)).rounded_full().bg(dot))
                    .child(div().text_color(t.text_secondary).child(state))
                    .child(server)
                    .when_some(cfg.ssh.as_ref(), |d, ssh| {
                        d.child(sep()).child(format!("via SSH tunnel {}", ssh.host))
                    })
                    .child(div().flex_1())
                    .when_some(s.status_context(cx), |d, c| d.child(c))
            }
        }
    }
}

impl Render for AppView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = tokens(cx);
        let content = match (&self.onboarding, self.active_session()) {
            (Some(onboarding), _) => onboarding.clone().into_any_element(),
            (None, Some(s)) => s.into_any_element(),
            (None, None) => self.home.clone().into_any_element(),
        };
        v_flex()
            .size_full()
            .bg(t.canvas)
            .text_color(t.text)
            .key_context("App")
            .track_focus(&self.focus)
            .child(self.render_title_bar(cx))
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .child(self.render_rail(cx))
                    .child(div().flex_1().min_w_0().h_full().child(content)),
            )
            .child(self.render_status_bar(cx))
    }
}

/// "⌘1"… for the first nine open connections.
pub fn shortcut_hint(i: usize) -> Option<String> {
    let m = if cfg!(target_os = "macos") {
        "⌘"
    } else {
        "Ctrl+"
    };
    (i < 9).then(|| format!("{m}{}", i + 1))
}

impl AppView {
    /// Onboarding finished or skipped: on to the app.
    pub fn end_onboarding(&mut self, cx: &mut Context<Self>) {
        self.onboarding = None;
        cx.notify();
    }

    /// Starts onboarding as on a fresh install (tests, screenshots).
    #[cfg(test)]
    pub fn start_onboarding(&mut self, cx: &mut Context<Self>) -> Entity<Onboarding> {
        let me = cx.entity().downgrade();
        let home = self.home.clone();
        let view = cx.new(|cx| Onboarding::new(me, home, cx));
        self.onboarding = Some(view.clone());
        cx.notify();
        view
    }

    /// Ids of the open connections, in rail order.
    pub fn open_ids(&self, cx: &App) -> Vec<String> {
        self.sessions
            .iter()
            .map(|s| s.read(cx).shared.cfg.id.clone())
            .collect()
    }

    /// ⌘1…⌘9: show the n-th open connection (rail order).
    pub fn activate_session(&mut self, i: usize, cx: &mut Context<Self>) {
        if i < self.sessions.len() {
            self.active = Some(i);
            cx.notify();
        }
    }

    /// ⌘N / rail "+" / sidebar header: switch to an open connection, open a
    /// saved one, or create a new one.
    pub fn open_connection_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let mut items = Vec::new();
        let mut open_ids = Vec::new();
        for (i, s) in self.sessions.iter().enumerate() {
            let cfg = &s.read(cx).shared.cfg;
            open_ids.push(cfg.id.clone());
            items.push(PickItem {
                detail: Some(cfg.driver.label().to_string()),
                dot: Some(tag_color(cfg, cx)),
                current: self.active == Some(i),
                group: Some("Open now".into()),
                hint: shortcut_hint(i),
                ..PickItem::new(format!("s:{i}"), cfg.display_name())
            });
        }
        // Saved ones by when they were last used, newest first.
        let mut saved: Vec<&ConnectionConfig> = self
            .home
            .read(cx)
            .connections
            .iter()
            .filter(|c| !open_ids.contains(&c.id))
            .collect();
        saved.sort_by_key(|c| Reverse(c.last_used.unwrap_or(0)));
        for cfg in saved {
            let place = match cfg.driver {
                crate::db::Driver::Sqlite => cfg.file.clone(),
                _ => cfg.host.clone(),
            };
            items.push(PickItem {
                detail: Some(format!("{} · {place}", cfg.driver.label())),
                dot: Some(tag_color(cfg, cx)),
                group: Some("Saved".into()),
                ..PickItem::new(format!("c:{}", cfg.id), cfg.display_name())
            });
        }
        items.push(PickItem {
            icon: Some(IconName::Plus),
            group: Some("New".into()),
            hint: Some("⌘N".into()),
            ..PickItem::new("new", "New connection…")
        });
        let me = cx.entity().downgrade();
        let on_pick: OnPick = Rc::new(move |id, window, cx| {
            me.update(cx, |a, cx| a.pick_connection(&id, window, cx))
                .ok();
        });
        Picker::open(
            "Connections",
            "Search connections…",
            IconName::Database,
            items,
            on_pick,
            window,
            cx,
        );
    }

    fn pick_connection(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(i) = id.strip_prefix("s:").and_then(|n| n.parse().ok()) {
            self.activate_session(i, cx);
            return;
        }
        // Connecting and the form both call back into this view, which is
        // mid-update here, so run them once this update has finished.
        let home = self.home.clone();
        if let Some(cid) = id.strip_prefix("c:") {
            let cfg = self
                .home
                .read(cx)
                .connections
                .iter()
                .find(|c| c.id == cid)
                .cloned();
            if let Some(cfg) = cfg {
                window.defer(cx, move |window, cx| {
                    home.update(cx, |h, cx| h.connect(cfg, window, cx))
                });
            }
        } else if id == "new" {
            window.defer(cx, move |window, cx| {
                home.update(cx, |h, cx| h.open_form(None, window, cx))
            });
        }
    }
}

fn toggle_theme(window: &mut Window, cx: &mut App) {
    let mode = if cx.theme().mode.is_dark() {
        ThemeMode::Light
    } else {
        ThemeMode::Dark
    };
    Theme::change(mode, Some(window), cx);
    // Remembered, so the next launch opens the same way. This also puts
    // the user's sizes back (a theme switch can reload them).
    settings::update(cx, |s| {
        s.appearance = match mode {
            ThemeMode::Light => settings::Appearance::Light,
            ThemeMode::Dark => settings::Appearance::Dark,
        }
    });
}

/// How a connection attempt ended: the error text when it failed.
pub type Connected = Result<(), String>;

/// The window's root view, so app-wide shortcuts can reach it.
struct AppHandle {
    view: WeakEntity<AppView>,
    window: AnyWindowHandle,
}
impl Global for AppHandle {}

/// Runs `f` on the app view of the focused window. Skipped while a dialog is
/// open so shortcuts don't act on what's behind it.
///
/// Global action handlers run while gpui is still dispatching the keystroke,
/// with the window checked out, so `update` on it would fail. Deferring runs
/// `f` right after dispatch, once the window is available again.
fn with_app(
    cx: &mut App,
    f: impl FnOnce(&mut AppView, &mut Window, &mut Context<AppView>) + 'static,
) {
    // Not `cx.active_window()`: it's empty while a keystroke is dispatched.
    let Some((app, win)) = cx
        .try_global::<AppHandle>()
        .and_then(|h| Some((h.view.upgrade()?, h.window)))
    else {
        return;
    };
    cx.defer(move |cx| {
        let res = win.update(cx, |_, window, cx| {
            if !window.has_active_dialog(cx) {
                app.update(cx, |a, cx| f(a, window, cx));
            }
        });
        if let Err(e) = res {
            eprintln!("tabula: shortcut couldn't reach the window: {e:#}");
        }
    });
}

/// Runs `f` on the connection currently shown, if any.
fn with_session(
    cx: &mut App,
    f: impl FnOnce(&mut Session, &mut Window, &mut Context<Session>) + 'static,
) {
    with_app(cx, |a, window, cx| {
        if let Some(s) = a.active.and_then(|i| a.sessions.get(i)).cloned() {
            s.update(cx, |s, cx| f(s, window, cx));
        }
    });
}

fn with_table(
    cx: &mut App,
    f: impl FnOnce(&mut TableTab, &mut Window, &mut Context<TableTab>) + 'static,
) {
    with_session(cx, |s, window, cx| {
        if let Some(t) = s.active_table() {
            t.update(cx, |t, cx| f(t, window, cx));
        }
    });
}

/// Shortcuts work wherever focus is: each one acts on the current
/// connection / tab rather than on the focused element.
pub fn register_actions(cx: &mut App) {
    cx.on_action(|_: &GoHome, cx| {
        with_app(cx, |a, _, cx| {
            a.active = None;
            cx.notify();
        })
    });
    cx.on_action(|_: &OpenConnection, cx| with_app(cx, |a, w, cx| a.open_connection_picker(w, cx)));
    fn go_to(cx: &mut App, i: usize) {
        with_app(cx, move |a, _, cx| a.activate_session(i, cx));
    }
    cx.on_action(|_: &Connection1, cx| go_to(cx, 0));
    cx.on_action(|_: &Connection2, cx| go_to(cx, 1));
    cx.on_action(|_: &Connection3, cx| go_to(cx, 2));
    cx.on_action(|_: &Connection4, cx| go_to(cx, 3));
    cx.on_action(|_: &Connection5, cx| go_to(cx, 4));
    cx.on_action(|_: &Connection6, cx| go_to(cx, 5));
    cx.on_action(|_: &Connection7, cx| go_to(cx, 6));
    cx.on_action(|_: &Connection8, cx| go_to(cx, 7));
    cx.on_action(|_: &Connection9, cx| go_to(cx, 8));
    cx.on_action(|_: &ToggleTheme, cx| with_app(cx, |_, w, cx| toggle_theme(w, cx)));
    cx.on_action(|_: &OpenSettings, cx| {
        with_app(cx, |_, w, cx| {
            SettingsView::open(w, cx);
        })
    });

    cx.on_action(|_: &NewQuery, cx| with_session(cx, |s, w, cx| s.new_query(w, cx)));
    cx.on_action(|_: &CloseTab, cx| with_session(cx, |s, w, cx| s.close_active_tab(w, cx)));
    cx.on_action(|_: &NextTab, cx| with_session(cx, |s, w, cx| s.cycle_tab(1, w, cx)));
    cx.on_action(|_: &PrevTab, cx| with_session(cx, |s, w, cx| s.cycle_tab(-1, w, cx)));
    cx.on_action(|_: &Refresh, cx| with_session(cx, |s, w, cx| s.refresh(w, cx)));
    cx.on_action(|_: &SwitchDatabase, cx| with_session(cx, |s, w, cx| s.open_db_picker(w, cx)));
    cx.on_action(|_: &GoTo, cx| {
        with_app(cx, |a, w, cx| match a.active_session() {
            Some(s) => s.update(cx, |s, cx| s.open_goto(w, cx)),
            None => a.open_connection_picker(w, cx),
        })
    });
    cx.on_action(|_: &RunQuery, cx| {
        with_session(cx, |s, w, cx| {
            if let Some(q) = s.active_query() {
                q.update(cx, |q, cx| q.run(&RunQuery, w, cx));
            }
        })
    });
    cx.on_action(|_: &RunAll, cx| {
        with_session(cx, |s, w, cx| {
            if let Some(q) = s.active_query() {
                q.update(cx, |q, cx| q.run_all(w, cx));
            }
        })
    });

    cx.on_action(|_: &QueryHistory, cx| with_session(cx, |s, w, cx| s.open_history(w, cx)));
    cx.on_action(|_: &ExplainQuery, cx| {
        with_session(cx, |s, w, cx| {
            if let Some(q) = s.active_query() {
                q.update(cx, |q, cx| q.explain(false, w, cx));
            }
        })
    });
    cx.on_action(|_: &NewDatabase, cx| with_session(cx, |s, w, cx| s.new_database(w, cx)));
    cx.on_action(|_: &DropDatabase, cx| with_session(cx, |s, w, cx| s.drop_database(w, cx)));
    cx.on_action(|_: &BackupDatabase, cx| with_session(cx, |s, w, cx| s.backup(w, cx)));
    cx.on_action(|_: &RestoreDatabase, cx| with_session(cx, |s, w, cx| s.restore(w, cx)));
    cx.on_action(|_: &ShowProcesses, cx| {
        with_session(cx, |s, w, cx| {
            super::processes::Processes::open(s.shared.clone(), w, cx)
        })
    });
    cx.on_action(|_: &SavedQueries, cx| with_session(cx, |s, w, cx| s.open_saved(w, cx)));
    cx.on_action(|_: &SaveQuery, cx| {
        with_session(cx, |s, w, cx| {
            if let Some(q) = s.active_query() {
                q.update(cx, |q, cx| q.save(w, cx));
            }
        })
    });
    cx.on_action(|_: &FormatSql, cx| {
        with_session(cx, |s, w, cx| {
            if let Some(q) = s.active_query() {
                q.update(cx, |q, cx| q.format(w, cx));
            }
        })
    });
    cx.on_action(|_: &ToggleRowDetail, cx| {
        with_session(cx, |s, _, cx| {
            if let Some(t) = s.active_table() {
                t.update(cx, |t, cx| {
                    t.show_detail = !t.show_detail;
                    cx.notify();
                });
            } else if let Some(q) = s.active_query() {
                q.update(cx, |q, cx| {
                    q.show_detail = !q.show_detail;
                    cx.notify();
                });
            }
        })
    });
    cx.on_action(|_: &ExportData, cx| {
        with_session(cx, |s, w, cx| {
            if let Some(t) = s.active_table() {
                t.update(cx, |t, cx| super::copy::open_export(t, w, cx));
            } else if let Some(q) = s.active_query() {
                q.update(cx, |q, cx| super::copy::open_export(q, w, cx));
            }
        })
    });
    cx.on_action(|_: &CancelQuery, cx| {
        with_session(cx, |s, _, cx| {
            if let Some(q) = s.active_query() {
                q.update(cx, |q, cx| q.cancel(cx));
            }
        })
    });

    // ⌘S / ⌘⇧⌫ act on the active table, or on an editable query result.
    cx.on_action(|_: &Commit, cx| {
        with_session(cx, |s, w, cx| {
            if let Some(t) = s.active_table() {
                t.update(cx, |t, cx| t.commit(&Commit, w, cx));
            } else if let Some(q) = s.active_query() {
                q.update(cx, |q, cx| q.commit(w, cx));
            }
        })
    });
    cx.on_action(|_: &Discard, cx| {
        with_session(cx, |s, w, cx| {
            if let Some(t) = s.active_table() {
                t.update(cx, |t, cx| t.discard(&Discard, w, cx));
            } else if let Some(q) = s.active_query() {
                q.update(cx, |q, cx| q.discard(cx));
            }
        })
    });
    cx.on_action(|_: &ToggleFilter, cx| {
        with_table(cx, |t, w, cx| t.toggle_filter(&ToggleFilter, w, cx))
    });
    cx.on_action(|_: &AddRow, cx| with_table(cx, |t, w, cx| t.add_row(&AddRow, w, cx)));
    cx.on_action(|_: &DeleteRows, cx| with_table(cx, |t, w, cx| t.delete_rows(&DeleteRows, w, cx)));
}

/// ⌘= / ⌘−: app-wide, so they also work while a dialog is open.
pub fn zoom(delta: f32, cx: &mut App) {
    settings::update(cx, |s| {
        s.ui_font_size += delta;
        s.editor_font_size += delta;
    });
}

#[cfg(test)]
mod shots;

#[cfg(test)]
mod tests {
    use std::env::temp_dir;
    use std::fs::{remove_file, write};
    use std::path::Path;
    use std::time::{Duration, Instant};

    use gpui::{TestAppContext, VisualTestContext};
    use gpui_component::Root;
    use gpui_component::menu::PopupMenu;

    use super::*;
    use crate::db::{ConnectionConfig, Driver, EnvTag};
    use crate::settings::{Appearance, Settings};
    use crate::ui::mongo_views::DocView;
    use crate::ui::onboarding::Step;
    use chrono::NaiveTime;
    use futures::executor::block_on;
    use gpui::{ClipboardItem, Modifiers};
    use gpui_component::init;
    use serde_json::to_string;

    /// Opens the real app view in a test window, with app-wide shortcuts.
    fn open(cx: &mut TestAppContext) -> (Entity<AppView>, VisualTestContext) {
        cx.update(|cx| {
            init(cx);
            cx.bind_keys(crate::ui::key_bindings());
            register_actions(cx);
            // Tests run as an install that has seen the "Passwords" step;
            // `passwords_step_comes_first` covers the step itself.
            cx.set_global(Settings {
                keychain_explained: true,
                ..Settings::default()
            });
        });
        let mut app = None;
        let window = cx.add_window(|window, cx| {
            let view = cx.new(|cx| AppView::new(window, cx));
            app = Some(view.clone());
            Root::new(view, window, cx)
        });
        let vcx = VisualTestContext::from_window(window.into(), cx);
        vcx.run_until_parked();
        (app.unwrap(), vcx)
    }

    /// Waits for work on the tokio runtime (DB calls) to land in the UI.
    fn wait_until(
        cx: &mut VisualTestContext,
        what: &str,
        mut done: impl FnMut(&mut VisualTestContext) -> bool,
    ) {
        let start = Instant::now();
        while !done(cx) {
            assert!(
                start.elapsed() < Duration::from_secs(10),
                "timed out waiting for {what}"
            );
            std::thread::sleep(Duration::from_millis(20));
            cx.run_until_parked();
        }
    }

    #[gpui::test]
    fn shortcuts_work_with_nothing_focused(cx: &mut TestAppContext) {
        let (_, mut cx) = open(cx);
        cx.update(|window, cx| window.blur(cx));
        let dark = cx.update(|_, cx| cx.theme().mode.is_dark());

        cx.simulate_keystrokes("cmd-shift-l");
        cx.run_until_parked();
        assert_ne!(
            cx.update(|_, cx| cx.theme().mode.is_dark()),
            dark,
            "⌘⇧L toggles the theme"
        );

        cx.simulate_keystrokes("cmd-n");
        cx.run_until_parked();
        assert!(
            cx.update(|w, cx| w.has_active_dialog(cx)),
            "⌘N opens the new-connection dialog"
        );

        // With a dialog open, shortcuts don't act on what's behind it.
        cx.simulate_keystrokes("cmd-shift-l");
        cx.run_until_parked();
        assert_ne!(cx.update(|_, cx| cx.theme().mode.is_dark()), dark);
    }

    #[gpui::test]
    fn session_shortcuts_route_to_the_open_connection(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let (app, mut cx) = open(cx);
        let mut cfg = ConnectionConfig::new(Driver::Sqlite);
        let file = std::env::temp_dir().join(format!("tabula-ui-{}.db", cfg.id));
        std::fs::write(&file, b"").unwrap();
        cfg.file = file.to_string_lossy().into();
        app.update_in(&mut cx, |a, w, cx| a.connect(cfg, w, cx, |_, _, _| {}));
        wait_until(&mut cx, "connection", |cx| {
            app.read_with(cx, |a, _| a.sessions.len() == 1)
        });
        let session = app.read_with(&cx, |a, _| a.sessions[0].clone());
        let tabs = |cx: &mut VisualTestContext| session.read_with(cx, |s, _| s.tab_count());

        cx.update(|window, cx| window.blur(cx));
        cx.simulate_keystrokes("cmd-t");
        cx.run_until_parked();
        assert_eq!(
            tabs(&mut cx),
            1,
            "⌘T opens a query tab with nothing focused"
        );

        cx.simulate_keystrokes("cmd-t");
        cx.run_until_parked();
        assert_eq!(tabs(&mut cx), 2, "⌘T works again with the editor focused");

        cx.update(|window, cx| window.blur(cx));
        cx.simulate_keystrokes("cmd-w");
        cx.run_until_parked();
        assert_eq!(tabs(&mut cx), 1, "⌘W closes the active tab");
        let _ = std::fs::remove_file(file);
    }

    /// GitHub #11: ⌘K must focus the search box, and picking must switch.
    /// Needs the throwaway Postgres container from compose.yaml (TABULA_IT=1,
    /// port 55432), which also creates the `tabula_it_other` database.
    #[gpui::test]
    fn db_picker_focuses_search_and_switches(cx: &mut TestAppContext) {
        if std::env::var("TABULA_IT").is_err() {
            return;
        }
        cx.executor().allow_parking();
        let (app, mut cx) = open(cx);
        let mut cfg = ConnectionConfig::new(Driver::Postgres);
        cfg.port = 55432;
        cfg.user = "postgres".into();
        cfg.password = "test".into();
        cfg.database = "postgres".into();
        app.update_in(&mut cx, |a, w, cx| a.connect(cfg, w, cx, |_, _, _| {}));
        wait_until(&mut cx, "connection", |cx| {
            app.read_with(cx, |a, _| a.sessions.len() == 1)
        });
        let session = app.read_with(&cx, |a, _| a.sessions[0].clone());
        wait_until(&mut cx, "database list", |cx| {
            session.read_with(cx, |s, _| s.database_count() > 1)
        });
        let current = |cx: &mut VisualTestContext| {
            session.read_with(cx, |s, _| {
                s.shared.conn.try_read().map(|c| c.current_database()).ok()
            })
        };
        assert_eq!(current(&mut cx).as_deref(), Some("postgres"));

        cx.update(|window, cx| window.blur(cx));
        cx.simulate_keystrokes("cmd-k");
        cx.run_until_parked();
        assert!(
            cx.update(|w, cx| w.has_active_dialog(cx)),
            "⌘K opens the picker"
        );
        assert!(
            cx.update(|w, cx| w.focused_input(cx).is_some()),
            "search box has focus"
        );

        cx.simulate_input("tabula_it_other");
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        assert!(
            !cx.update(|w, cx| w.has_active_dialog(cx)),
            "Enter closes the picker"
        );
        wait_until(&mut cx, "switch to tabula_it_other", |cx| {
            current(cx).as_deref() == Some("tabula_it_other")
        });

        // Clicking a database switches too. The one we came from is Recent.
        cx.simulate_keystrokes("cmd-k");
        cx.run_until_parked();
        assert!(
            cx.debug_bounds("pick-row-recent:postgres").is_some(),
            "previous database listed under Recent"
        );
        let row = cx
            .debug_bounds("pick-row-postgres")
            .expect("postgres row is shown");
        cx.simulate_click(row.center(), Modifiers::none());
        cx.run_until_parked();
        wait_until(&mut cx, "click to switch to postgres", |cx| {
            current(cx).as_deref() == Some("postgres")
        });

        // ↓ then Enter: the list starts on the current database.
        cx.simulate_keystrokes("cmd-k");
        cx.run_until_parked();
        cx.simulate_keystrokes("down enter");
        cx.run_until_parked();
        wait_until(&mut cx, "arrow + enter switch", |cx| {
            current(cx).as_deref() == Some("tabula_it_other")
        });
    }

    /// Review #3 / #4: with an uncommitted edit, paging doesn't reload (and
    /// lose it), and switching database asks first instead of closing the tab.
    #[gpui::test]
    fn unsaved_edits_survive_paging_and_db_switch(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let mut cfg = ConnectionConfig::new(Driver::Sqlite);
        let file = std::env::temp_dir().join(format!("tabula-ui-edits-{}.db", cfg.id));
        std::fs::write(&file, b"").unwrap();
        cfg.file = file.to_string_lossy().into();
        let setup = cfg.clone();
        block_on(crate::runtime::spawn(async move {
            let c = crate::db::Connection::connect(&setup).await?;
            c.execute("create table t (id integer primary key, a text); insert into t (a) values ('x'), ('y')")
                .await?;
            c.close().await;
            Ok(())
        }))
        .unwrap();

        let (app, mut cx) = open(cx);
        app.update_in(&mut cx, |a, w, cx| a.connect(cfg, w, cx, |_, _, _| {}));
        wait_until(&mut cx, "connection", |cx| {
            app.read_with(cx, |a, _| a.sessions.len() == 1)
        });
        let session = app.read_with(&cx, |a, _| a.sessions[0].clone());
        wait_until(&mut cx, "tables", |cx| {
            session.read_with(cx, |s, _| s.object_index("t").is_some())
        });
        session.update_in(&mut cx, |s, w, cx| {
            let i = s.object_index("t").unwrap();
            s.open_object(i, w, cx)
        });
        let tab = session.read_with(&cx, |s, _| s.active_table().unwrap());
        wait_until(&mut cx, "rows", |cx| {
            tab.read_with(cx, |t, cx| t.loaded_rows(cx) == 2)
        });

        tab.update(&mut cx, |t, cx| t.stage_cell(0, 1, "edited", cx));
        assert!(tab.read_with(&cx, |t, cx| t.is_dirty(cx)));

        tab.update_in(&mut cx, |t, w, cx| t.next_page_for_test(w, cx));
        cx.run_until_parked();
        assert_eq!(tab.read_with(&cx, |t, _| t.page()), 0, "page didn't change");
        assert!(tab.read_with(&cx, |t, cx| t.is_dirty(cx)), "edit kept");

        session.update_in(&mut cx, |s, w, cx| {
            s.request_switch_for_test("other", w, cx)
        });
        cx.run_until_parked();
        assert!(
            cx.update(|w, cx| w.has_active_dialog(cx)),
            "asks before discarding"
        );
        assert_eq!(
            session.read_with(&cx, |s, _| s.current_db().to_string()),
            "main"
        );
        assert!(
            session.read_with(&cx, |s, _| s.active_table().is_some()),
            "tab still open"
        );
        let _ = std::fs::remove_file(file);
    }

    /// TablePlus-style inline editing: type to start, Tab to save and move,
    /// Esc to cancel, Enter to edit the whole value, dialog for multi-line.
    #[gpui::test]
    fn inline_cell_editing(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let mut cfg = ConnectionConfig::new(Driver::Sqlite);
        let file = std::env::temp_dir().join(format!("tabula-ui-inline-{}.db", cfg.id));
        std::fs::write(&file, b"").unwrap();
        cfg.file = file.to_string_lossy().into();
        let setup = cfg.clone();
        block_on(crate::runtime::spawn(async move {
            let c = crate::db::Connection::connect(&setup).await?;
            c.execute(
                "create table t (id integer primary key, a text, b text); \
                 insert into t (a, b) values ('x', 'p'), ('y', 'q'), ('line1' || char(10) || 'line2', 'r')",
            )
            .await?;
            c.close().await;
            Ok(())
        }))
        .unwrap();

        let (app, mut cx) = open(cx);
        app.update_in(&mut cx, |a, w, cx| a.connect(cfg, w, cx, |_, _, _| {}));
        wait_until(&mut cx, "connection", |cx| {
            app.read_with(cx, |a, _| a.sessions.len() == 1)
        });
        let session = app.read_with(&cx, |a, _| a.sessions[0].clone());
        wait_until(&mut cx, "tables", |cx| {
            session.read_with(cx, |s, _| s.object_index("t").is_some())
        });
        session.update_in(&mut cx, |s, w, cx| {
            let i = s.object_index("t").unwrap();
            s.open_object(i, w, cx)
        });
        let tab = session.read_with(&cx, |s, _| s.active_table().unwrap());
        wait_until(&mut cx, "rows", |cx| {
            tab.read_with(cx, |t, cx| t.loaded_rows(cx) == 3)
        });
        let editing = |cx: &mut VisualTestContext| tab.read_with(cx, |t, cx| t.editing_cell(cx));
        let staged =
            |cx: &mut VisualTestContext, r, c| tab.read_with(cx, |t, cx| t.staged(r, c, cx));

        // Typing on a selected cell starts editing with that character.
        tab.update_in(&mut cx, |t, w, cx| t.select_cell(0, 1, w, cx));
        cx.simulate_keystrokes("z");
        cx.run_until_parked();
        assert_eq!(editing(&mut cx), Some((0, 1)));
        cx.simulate_input("ed");
        // Tab saves and moves to the next cell.
        cx.simulate_keystrokes("tab");
        cx.run_until_parked();
        assert_eq!(
            staged(&mut cx, 0, 1),
            Some(Some("zed".into())),
            "typed key not doubled"
        );
        assert_eq!(editing(&mut cx), Some((0, 2)));
        // Esc cancels: nothing staged, value unchanged.
        cx.simulate_input("nope");
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        assert_eq!(editing(&mut cx), None);
        assert_eq!(staged(&mut cx, 0, 2), None);

        // Enter edits with the whole value selected, so typing replaces it.
        tab.update_in(&mut cx, |t, w, cx| t.select_cell(1, 1, w, cx));
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        assert_eq!(editing(&mut cx), Some((1, 1)));
        cx.simulate_input("Q");
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        assert_eq!(editing(&mut cx), None);
        assert_eq!(staged(&mut cx, 1, 1), Some(Some("Q".into())));

        // Shift+Tab goes back a cell.
        tab.update_in(&mut cx, |t, w, cx| t.select_cell(1, 2, w, cx));
        cx.simulate_keystrokes("enter shift-tab");
        cx.run_until_parked();
        assert_eq!(editing(&mut cx), Some((1, 1)));
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();

        // Multi-line values open a dialog instead of being flattened.
        tab.update_in(&mut cx, |t, w, cx| t.select_cell(2, 1, w, cx));
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        assert_eq!(editing(&mut cx), None);
        assert!(
            cx.update(|w, cx| w.has_active_dialog(cx)),
            "multi-line editor opened"
        );
        let _ = std::fs::remove_file(file);
    }

    /// Values are checked against the column type while editing: ↵ won't
    /// save an invalid one. Space flips a boolean, ⌥⌫ sets NULL, ⌥↵ opens the
    /// bigger editor.
    #[gpui::test]
    fn typed_cell_editing(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let mut cfg = ConnectionConfig::new(Driver::Sqlite);
        let file = std::env::temp_dir().join(format!("tabula-ui-typed-{}.db", cfg.id));
        std::fs::write(&file, b"").unwrap();
        cfg.file = file.to_string_lossy().into();
        let setup = cfg.clone();
        block_on(crate::runtime::spawn(async move {
            let c = crate::db::Connection::connect(&setup).await?;
            c.execute(
                "create table t (id integer primary key, n integer, b boolean, s varchar(3)); \
                 insert into t (n, b, s) values (1, 1, 'abc')",
            )
            .await?;
            c.close().await;
            Ok(())
        }))
        .unwrap();

        let (app, mut cx) = open(cx);
        app.update_in(&mut cx, |a, w, cx| a.connect(cfg, w, cx, |_, _, _| {}));
        wait_until(&mut cx, "connection", |cx| {
            app.read_with(cx, |a, _| a.sessions.len() == 1)
        });
        let session = app.read_with(&cx, |a, _| a.sessions[0].clone());
        wait_until(&mut cx, "tables", |cx| {
            session.read_with(cx, |s, _| s.object_index("t").is_some())
        });
        session.update_in(&mut cx, |s, w, cx| {
            let i = s.object_index("t").unwrap();
            s.open_object(i, w, cx)
        });
        let tab = session.read_with(&cx, |s, _| s.active_table().unwrap());
        wait_until(&mut cx, "rows", |cx| {
            tab.read_with(cx, |t, cx| t.loaded_rows(cx) == 1)
        });
        let editing = |cx: &mut VisualTestContext| tab.read_with(cx, |t, cx| t.editing_cell(cx));
        let staged =
            |cx: &mut VisualTestContext, r, c| tab.read_with(cx, |t, cx| t.staged(r, c, cx));

        // An integer column refuses text: ↵ keeps the editor open.
        tab.update_in(&mut cx, |t, w, cx| t.select_cell(0, 1, w, cx));
        cx.simulate_keystrokes("x");
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        assert_eq!(editing(&mut cx), Some((0, 1)), "invalid value not saved");
        assert_eq!(staged(&mut cx, 0, 1), None);
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        assert_eq!(editing(&mut cx), None);

        // A valid number saves; ↑ / ↓ step it (⇧ by 10).
        tab.update_in(&mut cx, |t, w, cx| t.select_cell(0, 1, w, cx));
        cx.simulate_keystrokes("4 2 up up shift-down enter");
        cx.run_until_parked();
        assert_eq!(staged(&mut cx, 0, 1), Some(Some("34".into())));

        // Space flips a boolean in the column's own spelling.
        tab.update_in(&mut cx, |t, w, cx| t.select_cell(0, 2, w, cx));
        cx.simulate_keystrokes("space");
        cx.run_until_parked();
        assert_eq!(editing(&mut cx), None);
        assert_eq!(staged(&mut cx, 0, 2), Some(Some("0".into())));

        // ⌥⌫ sets NULL.
        tab.update_in(&mut cx, |t, w, cx| t.select_cell(0, 3, w, cx));
        cx.simulate_keystrokes("alt-backspace");
        cx.run_until_parked();
        assert_eq!(staged(&mut cx, 0, 3), Some(None));

        // ⌥↵ opens the bigger editor.
        cx.simulate_keystrokes("alt-enter");
        cx.run_until_parked();
        assert!(cx.update(|w, cx| w.has_active_dialog(cx)));
        let _ = std::fs::remove_file(file);
    }

    /// The date editor's calendar, Generate (UUID) and Now (time) set the
    /// value with a click, and editing carries on until ↵.
    #[gpui::test]
    fn picker_buttons_in_cell_editors(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let mut cfg = ConnectionConfig::new(Driver::Sqlite);
        let file = std::env::temp_dir().join(format!("tabula-ui-pickers-{}.db", cfg.id));
        std::fs::write(&file, b"").unwrap();
        cfg.file = file.to_string_lossy().into();
        let setup = cfg.clone();
        block_on(crate::runtime::spawn(async move {
            let c = crate::db::Connection::connect(&setup).await?;
            c.execute(
                "create table t (id integer primary key, d date, ts timestamp, u uuid, tm time); \
                 insert into t (d, ts, u, tm) values ('2026-11-06', '2026-11-06 09:41:00', \
                   '5f0c2a1e-8b7d-4c3e-9a51-0d8f3b2e7c41', '10:00:00')",
            )
            .await?;
            c.close().await;
            Ok(())
        }))
        .unwrap();

        let (app, mut cx) = open(cx);
        app.update_in(&mut cx, |a, w, cx| a.connect(cfg, w, cx, |_, _, _| {}));
        wait_until(&mut cx, "connection", |cx| {
            app.read_with(cx, |a, _| a.sessions.len() == 1)
        });
        let session = app.read_with(&cx, |a, _| a.sessions[0].clone());
        wait_until(&mut cx, "tables", |cx| {
            session.read_with(cx, |s, _| s.object_index("t").is_some())
        });
        session.update_in(&mut cx, |s, w, cx| {
            let i = s.object_index("t").unwrap();
            s.open_object(i, w, cx)
        });
        let tab = session.read_with(&cx, |s, _| s.active_table().unwrap());
        wait_until(&mut cx, "rows", |cx| {
            tab.read_with(cx, |t, cx| t.loaded_rows(cx) == 1)
        });
        let editing = |cx: &mut VisualTestContext| tab.read_with(cx, |t, cx| t.editing_cell(cx));
        let staged =
            |cx: &mut VisualTestContext, r, c| tab.read_with(cx, |t, cx| t.staged(r, c, cx));
        let click = |cx: &mut VisualTestContext, what: &'static str| {
            let b = cx
                .debug_bounds(what)
                .unwrap_or_else(|| panic!("{what} not drawn"));
            cx.simulate_click(b.center(), Modifiers::none());
            cx.run_until_parked();
        };

        // A date: click the 15th, then ↵.
        tab.update_in(&mut cx, |t, w, cx| t.select_cell(0, 1, w, cx));
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        click(&mut cx, "cal-day-15");
        assert_eq!(
            editing(&mut cx),
            Some((0, 1)),
            "still editing after a click"
        );
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        assert_eq!(staged(&mut cx, 0, 1), Some(Some("2026-11-15".into())));

        // A timestamp keeps its time of day.
        tab.update_in(&mut cx, |t, w, cx| t.select_cell(0, 2, w, cx));
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        click(&mut cx, "cal-day-1");
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        assert_eq!(
            staged(&mut cx, 0, 2),
            Some(Some("2026-11-01 09:41:00".into()))
        );

        // A UUID: Generate makes a new one.
        tab.update_in(&mut cx, |t, w, cx| t.select_cell(0, 3, w, cx));
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        click(&mut cx, "uuid-generate");
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        let u = staged(&mut cx, 0, 3).flatten().expect("a new uuid staged");
        assert_ne!(u, "5f0c2a1e-8b7d-4c3e-9a51-0d8f3b2e7c41");
        assert_eq!(
            crate::ui::cell_value::normalize_uuid(&u).as_deref(),
            Some(u.as_str())
        );

        // A time: Now.
        tab.update_in(&mut cx, |t, w, cx| t.select_cell(0, 4, w, cx));
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        click(&mut cx, "time-now");
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        let tm = staged(&mut cx, 0, 4).flatten().expect("a time staged");
        assert!(NaiveTime::parse_from_str(&tm, "%H:%M:%S").is_ok(), "{tm}");
        let _ = std::fs::remove_file(file);
    }

    /// Editing a foreign-key cell lists matching rows of the referenced
    /// table (by key or name); ↓ ↵ or a click picks one.
    #[gpui::test]
    fn foreign_key_picker(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let mut cfg = ConnectionConfig::new(Driver::Sqlite);
        let file = std::env::temp_dir().join(format!("tabula-ui-fkpick-{}.db", cfg.id));
        std::fs::write(&file, b"").unwrap();
        cfg.file = file.to_string_lossy().into();
        let setup = cfg.clone();
        block_on(crate::runtime::spawn(async move {
            let c = crate::db::Connection::connect(&setup).await?;
            c.execute(
                "create table customers (id integer primary key, name text, email text); \
                 insert into customers values (1, 'Priya Raman', 'p@example.com'), \
                   (2, 'Marcus Lee', 'm@example.com'), (3, 'Ana Souza', 'a@example.com'); \
                 create table orders (id integer primary key, \
                   customer_id integer references customers(id)); \
                 insert into orders values (10, 1)",
            )
            .await?;
            c.close().await;
            Ok(())
        }))
        .unwrap();

        let (app, mut cx) = open(cx);
        app.update_in(&mut cx, |a, w, cx| a.connect(cfg, w, cx, |_, _, _| {}));
        wait_until(&mut cx, "connection", |cx| {
            app.read_with(cx, |a, _| a.sessions.len() == 1)
        });
        let session = app.read_with(&cx, |a, _| a.sessions[0].clone());
        wait_until(&mut cx, "tables", |cx| {
            session.read_with(cx, |s, _| s.object_index("orders").is_some())
        });
        session.update_in(&mut cx, |s, w, cx| {
            let i = s.object_index("orders").unwrap();
            s.open_object(i, w, cx)
        });
        let tab = session.read_with(&cx, |s, _| s.active_table().unwrap());
        wait_until(&mut cx, "rows", |cx| {
            tab.read_with(cx, |t, cx| t.loaded_rows(cx) == 1)
        });
        let matches =
            |cx: &mut VisualTestContext| tab.read_with(cx, |t, cx| t.fk_matches_for_test(cx));
        let staged =
            |cx: &mut VisualTestContext, r, c| tab.read_with(cx, |t, cx| t.staged(r, c, cx));

        // Typing a name finds the row; ↓ ↵ picks its key.
        tab.update_in(&mut cx, |t, w, cx| t.select_cell(0, 1, w, cx));
        cx.simulate_keystrokes("enter");
        cx.simulate_input("lee");
        wait_until(&mut cx, "matches", |cx| {
            matches(cx) == Some(vec!["2".into()])
        });
        cx.simulate_keystrokes("down enter");
        cx.run_until_parked();
        assert_eq!(staged(&mut cx, 0, 1), Some(Some("2".into())));

        // The editor opens searching for the current key; with the text
        // cleared it lists the first rows. A click picks one; ↵ saves.
        tab.update_in(&mut cx, |t, w, cx| t.select_cell(0, 1, w, cx));
        cx.simulate_keystrokes("enter");
        wait_until(&mut cx, "current key", |cx| {
            matches(cx) == Some(vec!["2".into()])
        });
        cx.simulate_keystrokes("backspace");
        wait_until(&mut cx, "all rows", |cx| {
            matches(cx).is_some_and(|m| m.len() == 3)
        });
        let b = cx.debug_bounds("fk-match-2").expect("third match drawn");
        cx.simulate_click(b.center(), Modifiers::none());
        cx.run_until_parked();
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        assert_eq!(staged(&mut cx, 0, 1), Some(Some("3".into())));
        let _ = std::fs::remove_file(file);
    }

    /// Safe mode: on production ↵ cancels the confirmation and ⌘↵ commits;
    /// "Confirm writes" can be snoozed for the connection.
    #[gpui::test]
    fn commit_confirmation(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let mut cfg = ConnectionConfig::new(Driver::Sqlite);
        cfg.tag = crate::db::EnvTag::Production;
        let file = std::env::temp_dir().join(format!("tabula-ui-confirm-{}.db", cfg.id));
        std::fs::write(&file, b"").unwrap();
        cfg.file = file.to_string_lossy().into();
        let setup = cfg.clone();
        let read_a = move || {
            let setup = setup.clone();
            block_on(crate::runtime::spawn(async move {
                let c = crate::db::Connection::connect(&setup).await?;
                let r = c.execute("select a from t").await?;
                c.close().await;
                Ok(r[0].rows[0][0].clone())
            }))
            .unwrap()
        };
        let setup = cfg.clone();
        block_on(crate::runtime::spawn(async move {
            let c = crate::db::Connection::connect(&setup).await?;
            c.execute(
                "create table t (id integer primary key, a text); insert into t (a) values ('x')",
            )
            .await?;
            c.close().await;
            Ok(())
        }))
        .unwrap();

        let (app, mut cx) = open(cx);
        app.update_in(&mut cx, |a, w, cx| a.connect(cfg, w, cx, |_, _, _| {}));
        wait_until(&mut cx, "connection", |cx| {
            app.read_with(cx, |a, _| a.sessions.len() == 1)
        });
        let session = app.read_with(&cx, |a, _| a.sessions[0].clone());
        wait_until(&mut cx, "tables", |cx| {
            session.read_with(cx, |s, _| s.object_index("t").is_some())
        });
        session.update_in(&mut cx, |s, w, cx| {
            let i = s.object_index("t").unwrap();
            s.open_object(i, w, cx)
        });
        let tab = session.read_with(&cx, |s, _| s.active_table().unwrap());
        wait_until(&mut cx, "rows", |cx| {
            tab.read_with(cx, |t, cx| t.loaded_rows(cx) == 1)
        });
        let dialog = |cx: &mut VisualTestContext| cx.update(|w, cx| w.has_active_dialog(cx));

        tab.update_in(&mut cx, |t, _, cx| t.stage_cell(0, 1, "y", cx));
        cx.simulate_keystrokes("cmd-s");
        cx.run_until_parked();
        assert!(dialog(&mut cx), "production asks first");
        // ↵ cancels on production.
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        assert!(!dialog(&mut cx));
        assert_eq!(read_a(), Some("x".into()), "nothing written");

        // ⌘↵ commits.
        cx.simulate_keystrokes("cmd-s");
        cx.run_until_parked();
        assert!(dialog(&mut cx));
        cx.simulate_keystrokes("cmd-enter");
        wait_until(&mut cx, "commit", |_| read_a().as_deref() == Some("y"));
        assert!(!dialog(&mut cx));

        // Snoozing: the next commit doesn't ask.
        // Loaded, not just showing "y": edits made mid-load are refused.
        wait_until(&mut cx, "reload", |cx| {
            tab.read_with(cx, |t, cx| {
                t.cell(0, 1, cx).as_deref() == Some("y") && !t.is_loading(cx)
            })
        });
        tab.update_in(&mut cx, |t, _, cx| t.stage_cell(0, 1, "z", cx));
        cx.simulate_keystrokes("cmd-s");
        cx.run_until_parked();
        let b = cx.debug_bounds("snooze").expect("snooze offered");
        cx.simulate_click(b.center(), Modifiers::none());
        cx.run_until_parked();
        cx.simulate_keystrokes("cmd-enter");
        wait_until(&mut cx, "commit z", |_| read_a().as_deref() == Some("z"));
        wait_until(&mut cx, "reload z", |cx| {
            tab.read_with(cx, |t, cx| {
                t.cell(0, 1, cx).as_deref() == Some("z") && !t.is_loading(cx)
            })
        });
        tab.update_in(&mut cx, |t, _, cx| t.stage_cell(0, 1, "w", cx));
        cx.simulate_keystrokes("cmd-s");
        cx.run_until_parked();
        assert!(!dialog(&mut cx), "snoozed");
        wait_until(&mut cx, "commit w", |_| read_a().as_deref() == Some("w"));
        let _ = std::fs::remove_file(file);
    }

    #[gpui::test]
    fn filters_stay_as_chips_until_removed(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let mut cfg = ConnectionConfig::new(Driver::Sqlite);
        let file = std::env::temp_dir().join(format!("tabula-ui-filter-{}.db", cfg.id));
        std::fs::write(&file, b"").unwrap();
        cfg.file = file.to_string_lossy().into();
        let setup = cfg.clone();
        block_on(crate::runtime::spawn(async move {
            let c = crate::db::Connection::connect(&setup).await?;
            c.execute("create table t (id integer primary key, a text); insert into t (a) values ('x'), ('y'), ('z')")
                .await?;
            c.close().await;
            Ok(())
        }))
        .unwrap();

        let (app, mut cx) = open(cx);
        app.update_in(&mut cx, |a, w, cx| a.connect(cfg, w, cx, |_, _, _| {}));
        wait_until(&mut cx, "connection", |cx| {
            app.read_with(cx, |a, _| a.sessions.len() == 1)
        });
        let session = app.read_with(&cx, |a, _| a.sessions[0].clone());
        wait_until(&mut cx, "tables", |cx| {
            session.read_with(cx, |s, _| s.object_index("t").is_some())
        });
        session.update_in(&mut cx, |s, w, cx| {
            let i = s.object_index("t").unwrap();
            s.open_object(i, w, cx)
        });
        let tab = session.read_with(&cx, |s, _| s.active_table().unwrap());
        let rows = |cx: &mut VisualTestContext| tab.read_with(cx, |t, cx| t.loaded_rows(cx));
        wait_until(&mut cx, "rows", |cx| rows(cx) == 3);

        // ⌘F opens the editor with one condition on the first column (id =).
        tab.update_in(&mut cx, |t, w, cx| {
            t.toggle_filter(&ToggleFilter, w, cx);
            t.set_filter_value_for_test(0, "2", w, cx);
        });
        wait_until(&mut cx, "filtered rows", |cx| rows(cx) == 1);
        assert_eq!(tab.read_with(&cx, |t, _| t.chips_for_test()), 1);

        // Hiding the editor keeps the filter.
        tab.update_in(&mut cx, |t, w, cx| t.toggle_filter(&ToggleFilter, w, cx));
        cx.run_until_parked();
        assert_eq!(rows(&mut cx), 1);
        assert_eq!(tab.read_with(&cx, |t, _| t.chips_for_test()), 1);

        // The chip's × removes it and shows every row again.
        tab.update_in(&mut cx, |t, w, cx| t.remove_filter_for_test(0, w, cx));
        wait_until(&mut cx, "all rows", |cx| rows(cx) == 3);
        assert_eq!(tab.read_with(&cx, |t, _| t.chips_for_test()), 0);
        let _ = std::fs::remove_file(file);
    }

    /// Home: an open connection's card switches to it (no second copy); the
    /// environment filter hides cards; `/` focuses the search.
    #[gpui::test]
    fn home_cards(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let (app, mut cx) = open(cx);
        let home = app.read_with(&cx, |a, _| a.home.clone());
        let mut files = vec![];
        let mut cfgs = vec![];
        for (name, tag) in [
            ("tabula-home-a", EnvTag::Local),
            ("tabula-home-b", EnvTag::Production),
        ] {
            let mut cfg = ConnectionConfig::new(Driver::Sqlite);
            cfg.name = name.into();
            cfg.tag = tag;
            let file = temp_dir().join(format!("{name}-{}.db", cfg.id));
            write(&file, b"").unwrap();
            cfg.file = file.to_string_lossy().into();
            files.push(file);
            cfgs.push(cfg);
        }
        home.update(&mut cx, |h, cx| {
            h.connections = cfgs.clone();
            cx.notify();
        });
        let first = cfgs[0].clone();
        app.update_in(&mut cx, |a, w, cx| a.connect(first, w, cx, |_, _, _| {}));
        wait_until(&mut cx, "connected", |cx| {
            app.read_with(cx, |a, _| a.sessions.len() == 1)
        });
        app.update(&mut cx, |a, cx| {
            a.active = None;
            cx.notify();
        });
        cx.run_until_parked();

        // One click on the open card shows it again.
        let b = cx.debug_bounds("conn-card-0").expect("card drawn");
        cx.simulate_click(b.center(), Modifiers::none());
        cx.run_until_parked();
        assert_eq!(
            app.read_with(&cx, |a, _| (a.active, a.sessions.len())),
            (Some(0), 1)
        );

        app.update(&mut cx, |a, cx| {
            a.active = None;
            cx.notify();
        });
        cx.run_until_parked();
        // Prod only: the local card goes away.
        let b = cx.debug_bounds("env-filter-4").expect("filter drawn");
        cx.simulate_click(b.center(), Modifiers::none());
        cx.run_until_parked();
        assert!(cx.debug_bounds("conn-card-0").is_none());
        assert!(cx.debug_bounds("conn-card-1").is_some());

        // `/` puts the cursor in the search box.
        cx.update(|window, cx| window.blur(cx));
        cx.run_until_parked();
        cx.simulate_keystrokes("/");
        cx.simulate_input("zzz");
        cx.run_until_parked();
        assert!(cx.debug_bounds("conn-card-1").is_none(), "search applied");
        for f in files {
            let _ = remove_file(f);
        }
    }

    /// Onboarding: ↵ moves on, choices apply at once, the sample database
    /// opens behind the last step, and Open Tabula ends it for good. Skip
    /// setup ends it too.
    #[gpui::test]
    fn onboarding_flow(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let (app, mut cx) = open(cx);
        let click = |cx: &mut VisualTestContext, what: &'static str| {
            cx.run_until_parked();
            let bounds = cx
                .debug_bounds(what)
                .unwrap_or_else(|| panic!("{what} drawn"));
            cx.simulate_click(bounds.center(), Modifiers::none());
            cx.run_until_parked();
        };
        let flow = app.update(&mut cx, |app, cx| app.start_onboarding(cx));
        let step = |cx: &mut VisualTestContext| flow.read_with(cx, |flow, _| flow.step());
        cx.run_until_parked();
        assert_eq!(step(&mut cx), Step::Welcome);

        cx.update(|window, cx| window.blur(cx));
        cx.run_until_parked();
        cx.simulate_keystrokes("enter");
        assert_eq!(step(&mut cx), Step::Look);
        click(&mut cx, "theme-card-2");
        let appearance = cx.update(|_, cx| settings::get(cx).appearance);
        assert_eq!(appearance, Appearance::Dark);
        click(&mut cx, "onboarding-next");
        assert_eq!(step(&mut cx), Step::Connections);

        click(&mut cx, "pick-sample");
        assert_eq!(step(&mut cx), Step::Done);
        wait_until(&mut cx, "sample opened behind", |cx| {
            app.read_with(cx, |a, _| a.sessions.len() == 1)
        });
        assert!(
            app.read_with(&cx, |a, _| a.onboarding.is_some()),
            "still showing"
        );
        click(&mut cx, "onboarding-finish");
        assert!(app.read_with(&cx, |a, _| a.onboarding.is_none()));
        assert!(cx.update(|_, cx| settings::get(cx).onboarded));

        // Skip setup from the first step.
        app.update(&mut cx, |app, cx| app.start_onboarding(cx));
        click(&mut cx, "onboarding-skip");
        assert!(app.read_with(&cx, |a, _| a.onboarding.is_none()));
    }

    /// "Try the sample database" opens it, saved once as a connection.
    #[gpui::test]
    fn sample_database_opens(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let (app, mut cx) = open(cx);
        let home = app.read_with(&cx, |a, _| a.home.clone());
        home.update_in(&mut cx, |h, w, cx| h.open_sample(w, cx));
        wait_until(&mut cx, "sample open", |cx| {
            app.read_with(cx, |a, _| a.sessions.len() == 1)
        });
        let session = app.read_with(&cx, |a, _| a.sessions[0].clone());
        wait_until(&mut cx, "sample tables", |cx| {
            session.read_with(cx, |s, _| s.object_index("orders").is_some())
        });
        let saved = |cx: &mut VisualTestContext| {
            home.read_with(cx, |h, _| {
                h.connections
                    .iter()
                    .filter(|c| c.name == "Sample shop")
                    .count()
            })
        };
        assert_eq!(saved(&mut cx), 1);
        // Again: no second copy.
        home.update_in(&mut cx, |h, w, cx| h.open_sample(w, cx));
        wait_until(&mut cx, "second open", |cx| {
            home.read_with(cx, |h, _| h.connecting.is_empty())
        });
        assert_eq!(saved(&mut cx), 1);
        assert_eq!(
            app.read_with(&cx, |a, _| a.sessions.len()),
            1,
            "not opened twice"
        );
    }

    /// The first time the keychain may be involved, the "Passwords" step
    /// comes before it; afterwards it doesn't. "Don't save passwords"
    /// turns the keychain off.
    #[gpui::test]
    fn passwords_step_comes_first(cx: &mut TestAppContext) {
        let (app, mut cx) = open(cx);
        let home = app.read_with(&cx, |a, _| a.home.clone());
        let fresh = |cx: &mut VisualTestContext| {
            cx.update(|_, cx| {
                cx.set_global(Settings {
                    keychain_explained: false,
                    ..Settings::default()
                })
            })
        };
        let close = |cx: &mut VisualTestContext| {
            cx.update(|w, cx| w.close_dialog(cx));
            cx.executor().advance_clock(Duration::from_secs(1));
            cx.run_until_parked();
        };

        fresh(&mut cx);
        home.update_in(&mut cx, |h, w, cx| h.open_form(None, w, cx));
        cx.run_until_parked();
        assert!(
            cx.debug_bounds("passwords-continue").is_some(),
            "step first"
        );
        assert!(cx.debug_bounds("engine-pg").is_none(), "form not yet");
        let b = cx.debug_bounds("passwords-continue").unwrap();
        cx.simulate_click(b.center(), Modifiers::none());
        cx.executor().advance_clock(Duration::from_secs(1));
        cx.run_until_parked();
        assert!(cx.debug_bounds("engine-pg").is_some(), "then the form");
        let s = cx.update(|_, cx| crate::settings::get(cx));
        assert!(s.keychain_explained && s.save_passwords);
        close(&mut cx);

        // Shown once.
        home.update_in(&mut cx, |h, w, cx| h.open_form(None, w, cx));
        cx.run_until_parked();
        assert!(cx.debug_bounds("passwords-continue").is_none());
        close(&mut cx);

        // SQLite needs no keychain: no step even on a fresh install.
        fresh(&mut cx);
        let lite = ConnectionConfig::new(Driver::Sqlite);
        home.update_in(&mut cx, |h, w, cx| h.open_form(Some(lite), w, cx));
        cx.run_until_parked();
        assert!(cx.debug_bounds("passwords-continue").is_none());
    }

    /// Launch offers last session instead of connecting: nothing opens until
    /// Reopen (↵); an unanswered offer survives quitting.
    #[gpui::test]
    fn welcome_back_offers_last_session(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let (app, mut cx) = open(cx);
        let home = app.read_with(&cx, |a, _| a.home.clone());
        let mut cfg = ConnectionConfig::new(Driver::Sqlite);
        cfg.name = "kept".into();
        let file = temp_dir().join(format!("tabula-ui-reopen-{}.db", cfg.id));
        write(&file, b"").unwrap();
        cfg.file = file.to_string_lossy().into();
        let state = crate::restore::State {
            sessions: vec![crate::restore::SessionState {
                conn_id: cfg.id.clone(),
                tabs: vec![],
                active_tab: 0,
            }],
            active: Some(cfg.id.clone()),
        };
        home.update(&mut cx, |h, cx| {
            h.connections = vec![cfg.clone()];
            h.offer_reopen(state, cx);
        });
        cx.run_until_parked();
        assert!(cx.debug_bounds("reopen-card").is_some(), "offer shown");
        assert_eq!(
            app.read_with(&cx, |a, _| a.sessions.len()),
            0,
            "nothing connects"
        );
        // Quitting now would keep the offered session.
        let kept = app.read_with(&cx, |a, cx| a.snapshot(cx));
        assert_eq!(kept.sessions.len(), 1);

        cx.update(|window, cx| window.blur(cx));
        cx.run_until_parked();
        cx.simulate_keystrokes("enter");
        wait_until(&mut cx, "reopened", |cx| {
            app.read_with(cx, |a, _| a.sessions.len() == 1)
        });
        assert!(home.read_with(&cx, |h, _| h.reopen_offer().is_none()));
        let _ = remove_file(file);
    }

    /// New Database… switches to the database it created.
    /// Needs the Postgres container (TABULA_IT=1).
    #[gpui::test]
    fn new_database_opens_it(cx: &mut TestAppContext) {
        if std::env::var("TABULA_IT").is_err() {
            return;
        }
        cx.executor().allow_parking();
        let (app, mut cx) = open(cx);
        let mut cfg = ConnectionConfig::new(Driver::Postgres);
        cfg.port = 55432;
        cfg.user = "postgres".into();
        cfg.password = "test".into();
        cfg.database = "postgres".into();
        app.update_in(&mut cx, |a, w, cx| {
            a.connect(cfg.clone(), w, cx, |_, _, _| {})
        });
        wait_until(&mut cx, "connection", |cx| {
            app.read_with(cx, |a, _| a.sessions.len() == 1)
        });
        let session = app.read_with(&cx, |a, _| a.sessions[0].clone());
        let name = format!("tabula_it_new_{}", &cfg.id[..8]);
        cx.update(|window, cx| window.blur(cx));
        cx.dispatch_action(NewDatabase);
        cx.run_until_parked();
        cx.simulate_input(&name);
        cx.simulate_keystrokes("enter");
        let n = name.clone();
        wait_until(&mut cx, "switched to the new database", move |cx| {
            session.read_with(cx, |s, _| s.current_db() == n)
        });
        // Clean up from a separate connection.
        block_on(crate::runtime::spawn(async move {
            let c = crate::db::Connection::connect(&cfg).await?;
            c.execute(&format!("drop database if exists \"{name}\" with (force)"))
                .await?;
            c.close().await;
            Ok(())
        }))
        .unwrap();
    }

    /// + Foreign key adds one (named after its column); Drop Foreign Key…
    /// confirms, then drops it. Needs the Postgres container (TABULA_IT=1).
    #[gpui::test]
    fn add_and_drop_foreign_key(cx: &mut TestAppContext) {
        if std::env::var("TABULA_IT").is_err() {
            return;
        }
        cx.executor().allow_parking();
        let mut cfg = ConnectionConfig::new(Driver::Postgres);
        cfg.port = 55432;
        cfg.user = "postgres".into();
        cfg.password = "test".into();
        cfg.database = "postgres".into();
        let tag = cfg.id[..8].to_string();
        let (people, pets) = (format!("ui_fk_people_{tag}"), format!("ui_fk_pets_{tag}"));
        let setup = (cfg.clone(), people.clone(), pets.clone());
        block_on(crate::runtime::spawn(async move {
            let (cfg, people, pets) = setup;
            let c = crate::db::Connection::connect(&cfg).await?;
            c.execute(&format!(
                "create table {people} (id int primary key); \
                 create table {pets} (id int primary key, owner_id int)"
            ))
            .await?;
            c.close().await;
            Ok(())
        }))
        .unwrap();
        let (app, mut cx) = open(cx);
        app.update_in(&mut cx, |a, w, cx| {
            a.connect(cfg.clone(), w, cx, |_, _, _| {})
        });
        wait_until(&mut cx, "connection", |cx| {
            app.read_with(cx, |a, _| a.sessions.len() == 1)
        });
        let session = app.read_with(&cx, |a, _| a.sessions[0].clone());
        let pets_name = pets.clone();
        wait_until(&mut cx, "tables", move |cx| {
            session.read_with(cx, |s, _| s.object_index(&pets_name).is_some())
        });
        let session = app.read_with(&cx, |a, _| a.sessions[0].clone());
        session.update_in(&mut cx, |s, w, cx| {
            let index = s.object_index(&pets).unwrap();
            s.open_object(index, w, cx)
        });
        let tab = session.read_with(&cx, |s, _| s.active_table().unwrap());
        wait_until(&mut cx, "structure", |cx| {
            tab.read_with(cx, |t, _| t.has_structure())
        });
        let key_dialog = tab
            .update_in(&mut cx, |t, w, cx| t.open_foreign_key_dialog(w, cx))
            .unwrap();
        key_dialog.update_in(&mut cx, |dialog, w, cx| {
            dialog.choose_column("owner_id", w, cx);
            dialog.set_reference(&people, "id", w, cx);
            dialog.set_on_delete(1, cx);
            dialog.create(w, cx);
        });
        let key_name = format!("{pets}_owner_id_fkey");
        let expected = key_name.clone();
        wait_until(&mut cx, "foreign key added", move |cx| {
            tab.read_with(cx, |t, _| t.foreign_key_names().contains(&expected))
        });
        cx.executor().advance_clock(Duration::from_secs(1));
        cx.run_until_parked();
        let tab = session.read_with(&cx, |s, _| s.active_table().unwrap());
        tab.update_in(&mut cx, |t, w, cx| {
            t.drop_foreign_key_for_test(&key_name, w, cx)
        });
        cx.run_until_parked();
        cx.simulate_keystrokes("enter");
        wait_until(&mut cx, "foreign key dropped", move |cx| {
            tab.read_with(cx, |t, _| t.foreign_key_names().is_empty())
        });
        block_on(crate::runtime::spawn(async move {
            let c = crate::db::Connection::connect(&cfg).await?;
            c.execute(&format!("drop table {pets}; drop table {people}"))
                .await?;
            c.close().await;
            Ok(())
        }))
        .unwrap();
    }

    /// Selecting a Redis key opens it in the key editor; a staged field is
    /// saved with HSET. Needs the Redis container (TABULA_IT=1).
    #[gpui::test]
    fn redis_key_editor_saves(cx: &mut TestAppContext) {
        if std::env::var("TABULA_IT").is_err() {
            return;
        }
        cx.executor().allow_parking();
        let mut cfg = ConnectionConfig::new(Driver::Redis);
        cfg.port = 56379;
        cfg.database = "7".into();
        let setup = cfg.clone();
        block_on(crate::runtime::spawn(async move {
            let c = crate::db::Connection::connect(&setup).await?;
            c.execute("FLUSHDB\nHSET kv:user name Ada age 36").await?;
            c.close().await;
            Ok(())
        }))
        .unwrap();
        let (app, mut cx) = open(cx);
        app.update_in(&mut cx, |a, w, cx| {
            a.connect(cfg.clone(), w, cx, |_, _, _| {})
        });
        wait_until(&mut cx, "connection", |cx| {
            app.read_with(cx, |a, _| a.sessions.len() == 1)
        });
        let session = app.read_with(&cx, |a, _| a.sessions[0].clone());
        wait_until(&mut cx, "keys", |cx| {
            session.read_with(cx, |s, _| s.object_index("keys").is_some())
        });
        session.update_in(&mut cx, |s, w, cx| {
            let index = s.object_index("keys").unwrap();
            s.open_object(index, w, cx)
        });
        let tab = session.read_with(&cx, |s, _| s.active_table().unwrap());
        wait_until(&mut cx, "rows", |cx| {
            tab.read_with(cx, |t, cx| t.loaded_rows(cx) == 1)
        });
        tab.update_in(&mut cx, |t, w, cx| t.select_cell(0, 0, w, cx));
        let key_view = tab.read_with(&cx, |t, _| t.redis_key_view().unwrap());
        wait_until(&mut cx, "key read", |cx| {
            key_view.read_with(cx, |v, _| {
                v.detail_for_test().is_some_and(|d| d.length == 2)
            })
        });
        assert_eq!(
            key_view.read_with(&cx, |v, _| v.shown_key().map(String::from)),
            Some("kv:user".into())
        );
        key_view.update_in(&mut cx, |v, w, cx| {
            v.edit_row_for_test(2, "city", "Paris", w, cx);
            v.save(w, cx);
        });
        wait_until(&mut cx, "saved", |cx| {
            key_view.read_with(cx, |v, _| {
                v.detail_for_test().is_some_and(|d| d.length == 3)
            })
        });
        block_on(crate::runtime::spawn(async move {
            let c = crate::db::Connection::connect(&cfg).await?;
            c.execute("FLUSHDB").await?;
            c.close().await;
            Ok(())
        }))
        .unwrap();
    }

    /// A MongoDB collection opens on Documents; JSON shows the page with
    /// staged edits; ⌘S saves them. Needs the MongoDB container (TABULA_IT=1).
    #[gpui::test]
    fn mongo_document_views(cx: &mut TestAppContext) {
        if std::env::var("TABULA_IT").is_err() {
            return;
        }
        cx.executor().allow_parking();
        let mut cfg = ConnectionConfig::new(Driver::Mongo);
        cfg.port = 57017;
        cfg.database = "tabula_ui_docs".into();
        let setup = cfg.clone();
        block_on(crate::runtime::spawn(async move {
            let c = crate::db::Connection::connect(&setup).await?;
            c.execute("db.orders.drop()").await.ok();
            c.execute(r#"db.orders.insertMany([{_id: 1, status: "paid", items: {count: 2}}])"#)
                .await?;
            c.close().await;
            Ok(())
        }))
        .unwrap();
        let (app, mut cx) = open(cx);
        app.update_in(&mut cx, |a, w, cx| {
            a.connect(cfg.clone(), w, cx, |_, _, _| {})
        });
        wait_until(&mut cx, "connection", |cx| {
            app.read_with(cx, |a, _| a.sessions.len() == 1)
        });
        let session = app.read_with(&cx, |a, _| a.sessions[0].clone());
        wait_until(&mut cx, "collections", |cx| {
            session.read_with(cx, |s, _| s.object_index("orders").is_some())
        });
        session.update_in(&mut cx, |s, w, cx| {
            let index = s.object_index("orders").unwrap();
            s.open_object(index, w, cx)
        });
        let tab = session.read_with(&cx, |s, _| s.active_table().unwrap());
        wait_until(&mut cx, "documents", |cx| {
            tab.read_with(cx, |t, cx| t.loaded_rows(cx) == 1 && !t.is_loading(cx))
        });
        assert_eq!(tab.read_with(&cx, |t, _| t.doc_view()), DocView::Documents);
        cx.run_until_parked();
        let status = tab.read_with(&cx, |t, _| {
            t.column_names().iter().position(|c| c == "status").unwrap()
        });
        tab.update_in(&mut cx, |t, _, cx| {
            t.stage_cell(0, status, "shipped", cx);
            t.set_doc_view_for_test(DocView::Json, cx);
        });
        cx.run_until_parked();
        let json = tab.read_with(&cx, |t, cx| t.page_json_for_test(cx));
        assert!(json.contains("\"status\": \"shipped\""), "{json}");
        assert!(
            json.contains("\"count\": 2"),
            "nested objects stay objects: {json}"
        );
        tab.update_in(&mut cx, |t, w, cx| t.commit(&Commit, w, cx));
        wait_until(&mut cx, "saved", |cx| {
            tab.read_with(cx, |t, cx| {
                !t.is_dirty(cx) && !t.is_loading(cx) && t.page_json_for_test(cx).contains("shipped")
            })
        });
        // The find bar: a filter document narrows the page; bad JSON says so.
        tab.update_in(&mut cx, |t, w, cx| {
            t.find_for_test(r#"{ status: "pending" }"#, w, cx)
        });
        wait_until(&mut cx, "found nothing", |cx| {
            tab.read_with(cx, |t, cx| !t.is_loading(cx) && t.loaded_rows(cx) == 0)
        });
        tab.update_in(&mut cx, |t, w, cx| {
            t.find_for_test(r#"{ "items.count": 2 }"#, w, cx)
        });
        wait_until(&mut cx, "found by a nested field", |cx| {
            tab.read_with(cx, |t, cx| !t.is_loading(cx) && t.loaded_rows(cx) == 1)
        });
        tab.update_in(&mut cx, |t, w, cx| t.find_for_test("{ status: ", w, cx));
        wait_until(&mut cx, "bad filter reported", |cx| {
            tab.read_with(cx, |t, _| {
                t.error_for_test()
                    .is_some_and(|error| error.contains("find filter"))
            })
        });
        block_on(crate::runtime::spawn(async move {
            let c = crate::db::Connection::connect(&cfg).await?;
            c.execute("db.dropDatabase()").await.ok();
            c.close().await;
            Ok(())
        }))
        .unwrap();
    }

    /// ⌘↵ runs the statement under the cursor (one result, no tab bar);
    /// ⌘⇧↵ runs them all; a result tab's × closes it.
    #[gpui::test]
    fn run_statement_run_all_close_tab(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let mut cfg = ConnectionConfig::new(Driver::Sqlite);
        let file = temp_dir().join(format!("tabula-ui-runone-{}.db", cfg.id));
        write(&file, b"").unwrap();
        cfg.file = file.to_string_lossy().into();
        let (app, mut cx) = open(cx);
        app.update_in(&mut cx, |a, w, cx| a.connect(cfg, w, cx, |_, _, _| {}));
        wait_until(&mut cx, "connection", |cx| {
            app.read_with(cx, |a, _| a.sessions.len() == 1)
        });
        let session = app.read_with(&cx, |a, _| a.sessions[0].clone());
        session.update_in(&mut cx, |s, w, cx| s.new_query_for_test(w, cx));
        cx.run_until_parked();
        cx.simulate_input("select 1 as a;\nselect 2 as b;");
        let q = session.read_with(&cx, |s, _| s.active_query().unwrap());
        let idle = |cx: &mut VisualTestContext| q.read_with(cx, |q, _| !q.running());

        // Cursor on the first line: only that statement runs.
        cx.simulate_keystrokes("cmd-up cmd-enter");
        wait_until(&mut cx, "first", idle);
        assert_eq!(q.read_with(&cx, |q, _| q.result_count()), 1);
        assert!(cx.debug_bounds("result-tab-0").is_none(), "no tabs for one");

        cx.simulate_keystrokes("cmd-shift-enter");
        wait_until(&mut cx, "all", idle);
        assert_eq!(q.read_with(&cx, |q, _| q.result_count()), 2);
        cx.run_until_parked();
        let b = cx.debug_bounds("close-result-0").expect("close button");
        cx.simulate_click(b.center(), Modifiers::none());
        cx.run_until_parked();
        assert_eq!(q.read_with(&cx, |q, _| q.result_count()), 1);
        assert_eq!(q.read_with(&cx, |q, cx| q.cell(0, 0, cx)), Some("2".into()));
        let _ = remove_file(file);
    }

    /// A failed connection keeps its reason on the home card until the next
    /// attempt.
    #[gpui::test]
    fn failed_connection_stays_on_its_card(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let (app, mut cx) = open(cx);
        let home = app.read_with(&cx, |a, _| a.home.clone());
        let mut cfg = ConnectionConfig::new(Driver::Postgres);
        cfg.name = "nowhere".into();
        // Nothing listens on port 1.
        cfg.port = 1;
        let id = cfg.id.clone();
        home.update(&mut cx, |h, _| h.connections = vec![cfg.clone()]);
        home.update_in(&mut cx, |h, w, cx| h.connect(cfg.clone(), w, cx));
        wait_until(&mut cx, "attempt over", |cx| {
            home.read_with(cx, |h, _| h.connecting.is_empty())
        });
        let why = home.read_with(&cx, |h, _| h.failure(&id));
        assert!(why.is_some(), "reason kept");
        assert_eq!(app.read_with(&cx, |a, _| a.sessions.len()), 0);
        // Trying again clears it while it runs.
        home.update_in(&mut cx, |h, w, cx| h.connect(cfg.clone(), w, cx));
        assert_eq!(home.read_with(&cx, |h, _| h.failure(&id)), None);
        wait_until(&mut cx, "second attempt", |cx| {
            home.read_with(cx, |h, _| h.connecting.is_empty())
        });
    }

    /// Pasting a connection URL fills the form; Save keeps the password
    /// out of the saved connection list.
    #[gpui::test]
    fn connection_url_fills_the_form(cx: &mut TestAppContext) {
        let (app, mut cx) = open(cx);
        let home = app.read_with(&cx, |a, _| a.home.clone());
        let save = |cx: &mut VisualTestContext| {
            cx.run_until_parked();
            let b = cx.debug_bounds("conn-save").expect("save drawn");
            cx.simulate_click(b.center(), Modifiers::none());
            // Let the dialog finish closing.
            cx.executor()
                .advance_clock(std::time::Duration::from_secs(1));
            cx.run_until_parked();
        };

        home.update_in(&mut cx, |h, w, cx| h.open_form(None, w, cx));
        cx.run_until_parked();
        // The URL field has focus on a new connection; a paste fills the form.
        cx.write_to_clipboard(ClipboardItem::new_string(
            "mysql://ada:s%40cret@db.internal:3307/shop?ssl-mode=REQUIRED".into(),
        ));
        cx.simulate_keystrokes("cmd-v");
        save(&mut cx);
        let c = home.read_with(&cx, |h, _| h.connections.last().cloned().unwrap());
        assert_eq!(c.driver, Driver::MySql);
        assert_eq!((c.host.as_str(), c.port), ("db.internal", 3307));
        assert_eq!((c.user.as_str(), c.database.as_str()), ("ada", "shop"));
        assert_eq!(c.name, "shop");
        assert!(c.ssl);
        assert_eq!(c.password, "s@cret");
        let json = to_string(&c).unwrap();
        assert!(!json.contains("s@cret"), "{json}");

        home.update_in(&mut cx, |h, w, cx| h.open_form(None, w, cx));
        cx.run_until_parked();
        // Typed: nothing happens until Enter.
        cx.simulate_input("sqlite:///tmp/tabula-url-test.db");
        cx.run_until_parked();
        assert_eq!(home.read_with(&cx, |h, _| h.connections.len()), 1);
        cx.simulate_keystrokes("enter");
        save(&mut cx);
        let c = home.read_with(&cx, |h, _| h.connections.last().cloned().unwrap());
        assert_eq!(c.driver, Driver::Sqlite);
        assert_eq!(c.file, "/tmp/tabula-url-test.db");
        assert_eq!(c.name, "tabula-url-test");
    }

    /// The export dialog writes the filtered rows by default, the whole
    /// table when asked, in the chosen format and to the typed path.
    #[gpui::test]
    fn export_dialog(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let mut cfg = ConnectionConfig::new(Driver::Sqlite);
        let file = std::env::temp_dir().join(format!("tabula-ui-export-{}.db", cfg.id));
        std::fs::write(&file, b"").unwrap();
        cfg.file = file.to_string_lossy().into();
        let out = std::env::temp_dir().join(format!("tabula-ui-export-{}", cfg.id));
        let setup = cfg.clone();
        block_on(crate::runtime::spawn(async move {
            let c = crate::db::Connection::connect(&setup).await?;
            c.execute("create table t (id integer primary key, a text); insert into t (a) values ('x'), ('y'), ('z')")
                .await?;
            c.close().await;
            Ok(())
        }))
        .unwrap();

        let (app, mut cx) = open(cx);
        app.update_in(&mut cx, |a, w, cx| a.connect(cfg, w, cx, |_, _, _| {}));
        wait_until(&mut cx, "connection", |cx| {
            app.read_with(cx, |a, _| a.sessions.len() == 1)
        });
        let session = app.read_with(&cx, |a, _| a.sessions[0].clone());
        wait_until(&mut cx, "tables", |cx| {
            session.read_with(cx, |s, _| s.object_index("t").is_some())
        });
        session.update_in(&mut cx, |s, w, cx| {
            let i = s.object_index("t").unwrap();
            s.open_object(i, w, cx)
        });
        let tab = session.read_with(&cx, |s, _| s.active_table().unwrap());
        let rows = |cx: &mut VisualTestContext| tab.read_with(cx, |t, cx| t.loaded_rows(cx));
        wait_until(&mut cx, "rows", |cx| rows(cx) == 3);
        tab.update_in(&mut cx, |t, w, cx| {
            t.toggle_filter(&ToggleFilter, w, cx);
            t.set_filter_value_for_test(0, "2", w, cx);
        });
        wait_until(&mut cx, "filtered rows", |cx| rows(cx) == 1);

        let click = |cx: &mut VisualTestContext, what: &'static str| {
            cx.run_until_parked();
            let b = cx
                .debug_bounds(what)
                .unwrap_or_else(|| panic!("{what} drawn"));
            cx.simulate_click(b.center(), Modifiers::none());
            cx.run_until_parked();
        };
        let export = |cx: &mut VisualTestContext, path: &std::path::Path| {
            click(cx, "export-path");
            cx.simulate_keystrokes("cmd-a backspace");
            cx.simulate_input(&path.to_string_lossy());
            cx.simulate_keystrokes("cmd-enter");
            let p = path.to_path_buf();
            wait_until(cx, "export file", move |_| p.exists());
            std::fs::read_to_string(path).unwrap()
        };

        // Default: CSV of the current filter.
        cx.dispatch_action(ExportData);
        let csv = out.with_extension("csv");
        assert_eq!(export(&mut cx, &csv), "id,a\n2,y\n");

        // TSV of the whole table, without the header row.
        cx.dispatch_action(ExportData);
        click(&mut cx, "export-format-tsv");
        click(&mut cx, "export-scope-1");
        click(&mut cx, "export-header");
        let tsv = out.with_extension("tsv");
        assert_eq!(export(&mut cx, &tsv), "1\tx\n2\ty\n3\tz\n");

        for f in [file, csv, tsv] {
            let _ = std::fs::remove_file(f);
        }
    }

    /// An unknown column gets "Did you mean …?", and Use fixes the editor.
    #[gpui::test]
    fn query_error_suggests_a_column(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let mut cfg = ConnectionConfig::new(Driver::Sqlite);
        let file = temp_dir().join(format!("tabula-ui-suggest-{}.db", cfg.id));
        write(&file, b"").unwrap();
        cfg.file = file.to_string_lossy().into();
        let setup = cfg.clone();
        block_on(crate::runtime::spawn(async move {
            let c = crate::db::Connection::connect(&setup).await?;
            c.execute("create table t (id integer primary key, total numeric)")
                .await?;
            c.close().await;
            Ok(())
        }))
        .unwrap();
        let (app, mut cx) = open(cx);
        app.update_in(&mut cx, |a, w, cx| a.connect(cfg, w, cx, |_, _, _| {}));
        wait_until(&mut cx, "connection", |cx| {
            app.read_with(cx, |a, _| a.sessions.len() == 1)
        });
        let session = app.read_with(&cx, |a, _| a.sessions[0].clone());
        wait_until(&mut cx, "tables", |cx| {
            session.read_with(cx, |s, _| s.object_index("t").is_some())
        });
        session.update_in(&mut cx, |s, w, cx| s.new_query_for_test(w, cx));
        let query = session.read_with(&cx, |s, _| s.active_query().unwrap());
        query.update_in(&mut cx, |q, w, cx| {
            q.type_and_run("select totl from t", w, cx)
        });
        wait_until(&mut cx, "failed", |cx| {
            query.read_with(cx, |q, _| !q.running())
        });
        cx.run_until_parked();
        let button = cx
            .debug_bounds("apply-suggestion")
            .expect("suggestion offered");
        cx.simulate_click(button.center(), Modifiers::none());
        cx.run_until_parked();
        assert_eq!(
            query.read_with(&cx, |q, cx| q.text(cx)),
            "select total from t"
        );
        let _ = remove_file(file);
    }

    /// A lost connection shows the banner; Retry now asks the server and,
    /// with it answering, the banner goes away.
    #[gpui::test]
    fn connection_lost_banner_retries(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let mut cfg = ConnectionConfig::new(Driver::Sqlite);
        let file = temp_dir().join(format!("tabula-ui-lost-{}.db", cfg.id));
        write(&file, b"").unwrap();
        cfg.file = file.to_string_lossy().into();
        let (app, mut cx) = open(cx);
        app.update_in(&mut cx, |a, w, cx| a.connect(cfg, w, cx, |_, _, _| {}));
        wait_until(&mut cx, "connection", |cx| {
            app.read_with(cx, |a, _| a.sessions.len() == 1)
        });
        let session = app.read_with(&cx, |a, _| a.sessions[0].clone());
        assert!(cx.debug_bounds("connection-lost").is_none());
        session.update(&mut cx, |s, cx| {
            s.shared
                .report("Lost the connection to the server: broken pipe");
            cx.notify();
        });
        cx.run_until_parked();
        let button = cx
            .debug_bounds("retry-connection-button")
            .expect("banner shown");
        cx.simulate_click(button.center(), Modifiers::none());
        wait_until(&mut cx, "reconnected", |cx| {
            session.read_with(cx, |s, _| s.shared.lost.borrow().is_none())
        });
        cx.run_until_parked();
        assert!(cx.debug_bounds("connection-lost").is_none());
        let _ = remove_file(file);
    }

    /// ⌘P: `.` scopes to columns, Tab cycles the scope chips (All →
    /// Tables), ↵ opens the table.
    #[gpui::test]
    fn palette_scopes(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let mut cfg = ConnectionConfig::new(Driver::Sqlite);
        let file = temp_dir().join(format!("tabula-ui-palette-{}.db", cfg.id));
        write(&file, b"").unwrap();
        cfg.file = file.to_string_lossy().into();
        let setup = cfg.clone();
        block_on(crate::runtime::spawn(async move {
            let c = crate::db::Connection::connect(&setup).await?;
            c.execute("create table orders (id integer primary key, total numeric)")
                .await?;
            c.close().await;
            Ok(())
        }))
        .unwrap();
        let (app, mut cx) = open(cx);
        app.update_in(&mut cx, |a, w, cx| a.connect(cfg, w, cx, |_, _, _| {}));
        wait_until(&mut cx, "connection", |cx| {
            app.read_with(cx, |a, _| a.sessions.len() == 1)
        });
        let session = app.read_with(&cx, |a, _| a.sessions[0].clone());
        wait_until(&mut cx, "columns", |cx| {
            session.read_with(cx, |s, _| {
                s.object_index("orders").is_some() && s.column_count() > 0
            })
        });
        cx.dispatch_action(GoTo);
        cx.run_until_parked();
        assert!(
            cx.debug_bounds("pick-row-col:1").is_none(),
            "columns wait for a search"
        );
        cx.simulate_input(".tot");
        cx.run_until_parked();
        assert!(
            cx.debug_bounds("pick-row-col:1").is_some(),
            ". searches columns"
        );
        assert!(cx.debug_bounds("pick-row-o:0").is_none());
        // Clear, then Tab: the Tables chip.
        for _ in 0..4 {
            cx.simulate_keystrokes("backspace");
        }
        cx.simulate_keystrokes("tab");
        cx.run_until_parked();
        assert!(cx.debug_bounds("pick-row-o:0").is_some());
        assert!(
            cx.debug_bounds("pick-row-cmd:0").is_none(),
            "commands are out of scope"
        );
        cx.simulate_keystrokes("enter");
        wait_until(&mut cx, "table opened", |cx| {
            session.read_with(cx, |s, _| s.active_table().is_some())
        });
        // ⌘↵ opens it again, in a second tab.
        cx.dispatch_action(GoTo);
        cx.run_until_parked();
        cx.simulate_keystrokes("tab");
        cx.simulate_keystrokes("cmd-enter");
        wait_until(&mut cx, "second tab", |cx| {
            session.read_with(cx, |s, _| s.tab_count() == 2)
        });
        let _ = remove_file(file);
    }

    /// Load file: a file's bytes go into a blob cell and, once committed,
    /// are in the database byte for byte.
    #[gpui::test]
    fn load_file_into_blob(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let mut cfg = ConnectionConfig::new(Driver::Sqlite);
        let file = temp_dir().join(format!("tabula-ui-blob-{}.db", cfg.id));
        write(&file, b"").unwrap();
        cfg.file = file.to_string_lossy().into();
        let setup = cfg.clone();
        block_on(crate::runtime::spawn(async move {
            let c = crate::db::Connection::connect(&setup).await?;
            c.execute("create table files (id integer primary key, data blob); insert into files (id) values (1)")
                .await?;
            c.close().await;
            Ok(())
        }))
        .unwrap();
        let payload: Vec<u8> = (0..=255).rev().collect();
        let source = temp_dir().join(format!("tabula-ui-blob-{}.bin", cfg.id));
        write(&source, &payload).unwrap();
        let (app, mut cx) = open(cx);
        app.update_in(&mut cx, |a, w, cx| {
            a.connect(cfg.clone(), w, cx, |_, _, _| {})
        });
        wait_until(&mut cx, "connection", |cx| {
            app.read_with(cx, |a, _| a.sessions.len() == 1)
        });
        let session = app.read_with(&cx, |a, _| a.sessions[0].clone());
        wait_until(&mut cx, "tables", |cx| {
            session.read_with(cx, |s, _| s.object_index("files").is_some())
        });
        session.update_in(&mut cx, |s, w, cx| {
            let index = s.object_index("files").unwrap();
            s.open_object(index, w, cx)
        });
        let tab = session.read_with(&cx, |s, _| s.active_table().unwrap());
        wait_until(&mut cx, "rows", |cx| {
            tab.read_with(cx, |t, cx| t.loaded_rows(cx) == 1 && !t.is_loading(cx))
        });
        let source_path = source.clone();
        tab.update_in(&mut cx, |t, w, cx| t.stage_file(0, 1, source_path, w, cx));
        wait_until(&mut cx, "staged", |cx| {
            tab.read_with(cx, |t, cx| t.is_dirty(cx))
        });
        tab.update_in(&mut cx, |t, w, cx| t.commit(&Commit, w, cx));
        wait_until(&mut cx, "committed", |cx| {
            tab.read_with(cx, |t, cx| !t.is_dirty(cx) && !t.is_loading(cx))
        });
        let stored = block_on(crate::runtime::spawn(async move {
            let c = crate::db::Connection::connect(&cfg).await?;
            let result = c.execute("select data from files").await?;
            c.close().await;
            Ok(result[0].rows[0][0].clone())
        }))
        .unwrap()
        .unwrap();
        assert_eq!(crate::db::binary::decode(&stored).unwrap(), payload);
        let _ = remove_file(file);
        let _ = remove_file(source);
    }

    /// A saved query shows in the sidebar as soon as it's saved.
    #[gpui::test]
    fn saved_queries_in_sidebar(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let mut cfg = ConnectionConfig::new(Driver::Sqlite);
        let file = temp_dir().join(format!("tabula-ui-saved-side-{}.db", cfg.id));
        write(&file, b"").unwrap();
        cfg.file = file.to_string_lossy().into();
        let conn_id = cfg.id.clone();
        let (app, mut cx) = open(cx);
        app.update_in(&mut cx, |a, w, cx| a.connect(cfg, w, cx, |_, _, _| {}));
        wait_until(&mut cx, "connection", |cx| {
            app.read_with(cx, |a, _| a.sessions.len() == 1)
        });
        let session = app.read_with(&cx, |a, _| a.sessions[0].clone());
        assert!(session.read_with(&cx, |s, _| s.saved_in_sidebar().is_empty()));
        let query = crate::saved::SavedQuery {
            id: format!("sidebar-{conn_id}"),
            name: "Failed payments".into(),
            sql: "select 1".into(),
            conn_id: Some(conn_id),
        };
        crate::saved::upsert(query.clone()).unwrap();
        cx.update(|_, cx| crate::ui::saved_changed(cx));
        cx.run_until_parked();
        assert_eq!(
            session.read_with(&cx, |s, _| s.saved_in_sidebar()),
            ["Failed payments"]
        );
        crate::saved::delete(&query.id).unwrap();
        let _ = remove_file(file);
    }

    /// "Rows referencing this": from a customer, its orders open in a tab
    /// filtered to that customer.
    #[gpui::test]
    fn rows_referencing_this(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let mut cfg = ConnectionConfig::new(Driver::Sqlite);
        let file = temp_dir().join(format!("tabula-ui-inbound-{}.db", cfg.id));
        write(&file, b"").unwrap();
        cfg.file = file.to_string_lossy().into();
        let setup = cfg.clone();
        block_on(crate::runtime::spawn(async move {
            let c = crate::db::Connection::connect(&setup).await?;
            c.execute(
                "create table customers (id integer primary key, name text); \
                 create table orders (id integer primary key, customer_id integer references customers(id)); \
                 insert into customers values (1, 'Ana'), (2, 'Ben'); \
                 insert into orders values (10, 1), (11, 2), (12, 2)",
            )
            .await?;
            c.close().await;
            Ok(())
        }))
        .unwrap();
        let (app, mut cx) = open(cx);
        app.update_in(&mut cx, |a, w, cx| a.connect(cfg, w, cx, |_, _, _| {}));
        wait_until(&mut cx, "connection", |cx| {
            app.read_with(cx, |a, _| a.sessions.len() == 1)
        });
        let session = app.read_with(&cx, |a, _| a.sessions[0].clone());
        wait_until(&mut cx, "tables", |cx| {
            session.read_with(cx, |s, _| s.object_index("customers").is_some())
        });
        session.update_in(&mut cx, |s, w, cx| {
            let index = s.object_index("customers").unwrap();
            s.open_object(index, w, cx)
        });
        let customers = session.read_with(&cx, |s, _| s.active_table().unwrap());
        wait_until(&mut cx, "customers", |cx| {
            customers.read_with(cx, |t, cx| t.has_structure() && t.loaded_rows(cx) == 2)
        });
        assert_eq!(
            customers.read_with(&cx, |t, _| t.inbound_labels()),
            ["orders (customer_id)"]
        );
        // Ben (row 1) has two orders.
        customers.update_in(&mut cx, |t, _, cx| t.open_referencing(1, 0, cx));
        wait_until(&mut cx, "orders tab", |cx| {
            session.read_with(cx, |s, cx| {
                s.active_table().is_some_and(|t| {
                    let t = t.read(cx);
                    t.object.name == "orders" && !t.is_loading(cx) && t.loaded_rows(cx) == 2
                })
            })
        });
        let _ = remove_file(file);
    }

    /// The row menu builds (with its Copy as submenus), and "Filter by this
    /// value" narrows to the cell's value, or to NULL.
    #[gpui::test]
    fn row_menu_and_filter_by_value(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let mut cfg = ConnectionConfig::new(Driver::Sqlite);
        let file = temp_dir().join(format!("tabula-ui-rowmenu-{}.db", cfg.id));
        write(&file, b"").unwrap();
        cfg.file = file.to_string_lossy().into();
        let setup = cfg.clone();
        block_on(crate::runtime::spawn(async move {
            let c = crate::db::Connection::connect(&setup).await?;
            c.execute("create table t (id integer primary key, a text); insert into t (a) values ('x'), ('y'), ('x'), (null)")
                .await?;
            c.close().await;
            Ok(())
        }))
        .unwrap();
        let (app, mut cx) = open(cx);
        app.update_in(&mut cx, |a, w, cx| a.connect(cfg, w, cx, |_, _, _| {}));
        wait_until(&mut cx, "connection", |cx| {
            app.read_with(cx, |a, _| a.sessions.len() == 1)
        });
        let session = app.read_with(&cx, |a, _| a.sessions[0].clone());
        wait_until(&mut cx, "tables", |cx| {
            session.read_with(cx, |s, _| s.object_index("t").is_some())
        });
        session.update_in(&mut cx, |s, w, cx| {
            let i = s.object_index("t").unwrap();
            s.open_object(i, w, cx)
        });
        let tab = session.read_with(&cx, |s, _| s.active_table().unwrap());
        let rows = |cx: &mut VisualTestContext| tab.read_with(cx, |t, cx| t.loaded_rows(cx));
        wait_until(&mut cx, "rows", |cx| rows(cx) == 4);

        let grid = tab.read_with(&cx, |t, _| t.grid_for_test());
        let on_menu = grid.read_with(&cx, |g, _| g.delegate().on_menu.clone().unwrap());
        cx.update(|window, cx| {
            PopupMenu::build(window, cx, |menu, window, cx| on_menu(0, menu, window, cx));
        });

        tab.update_in(&mut cx, |t, w, cx| t.filter_by_cell_for_test(0, 1, w, cx));
        wait_until(&mut cx, "x rows", |cx| rows(cx) == 2);
        tab.update_in(&mut cx, |t, w, cx| t.clear_filters(w, cx));
        wait_until(&mut cx, "all rows", |cx| rows(cx) == 4);
        tab.update_in(&mut cx, |t, w, cx| t.filter_by_cell_for_test(3, 1, w, cx));
        wait_until(&mut cx, "null row", |cx| rows(cx) == 1);
        let _ = remove_file(file);
    }

    /// Enum columns edit with a dropdown of their values: typing jumps to a
    /// value, arrows move (NULL is last), Enter picks, Esc cancels.
    /// Needs the Postgres container (TABULA_IT=1).
    #[gpui::test]
    fn enum_cells_edit_with_a_dropdown(cx: &mut TestAppContext) {
        if std::env::var("TABULA_IT").is_err() {
            return;
        }
        cx.executor().allow_parking();
        let mut cfg = ConnectionConfig::new(Driver::Postgres);
        cfg.host = "127.0.0.1".into();
        cfg.port = 55432;
        cfg.user = "postgres".into();
        cfg.password = "test".into();
        cfg.database = "postgres".into();
        let setup = cfg.clone();
        let sql = |text: &'static str| {
            let setup = setup.clone();
            block_on(crate::runtime::spawn(async move {
                let c = crate::db::Connection::connect(&setup).await?;
                c.execute(text).await?;
                c.close().await;
                Ok(())
            }))
            .unwrap()
        };
        sql(
            "drop table if exists it_ui_enum; drop type if exists it_ui_mood; \
             create type it_ui_mood as enum ('sad', 'ok', 'happy'); \
             create table it_ui_enum (id int primary key, mood it_ui_mood, label text); \
             insert into it_ui_enum values (1, 'sad', 'x')",
        );

        let (app, mut cx) = open(cx);
        app.update_in(&mut cx, |a, w, cx| a.connect(cfg, w, cx, |_, _, _| {}));
        wait_until(&mut cx, "connection", |cx| {
            app.read_with(cx, |a, _| a.sessions.len() == 1)
        });
        let session = app.read_with(&cx, |a, _| a.sessions[0].clone());
        wait_until(&mut cx, "tables", |cx| {
            session.read_with(cx, |s, _| s.object_index("it_ui_enum").is_some())
        });
        session.update_in(&mut cx, |s, w, cx| {
            let i = s.object_index("it_ui_enum").unwrap();
            s.open_object(i, w, cx)
        });
        let tab = session.read_with(&cx, |s, _| s.active_table().unwrap());
        wait_until(&mut cx, "rows", |cx| {
            tab.read_with(cx, |t, cx| t.loaded_rows(cx) == 1)
        });
        let editing = |cx: &mut VisualTestContext| tab.read_with(cx, |t, cx| t.editing_cell(cx));
        let staged =
            |cx: &mut VisualTestContext, r, c| tab.read_with(cx, |t, cx| t.staged(r, c, cx));

        // A typed letter picks the value starting with it, not text "h".
        tab.update_in(&mut cx, |t, w, cx| t.select_cell(0, 1, w, cx));
        cx.simulate_keystrokes("h");
        cx.run_until_parked();
        assert_eq!(editing(&mut cx), Some((0, 1)));
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        assert_eq!(editing(&mut cx), None);
        assert_eq!(staged(&mut cx, 0, 1), Some(Some("happy".into())));

        // Opens on the current value; Down from the last value reaches NULL.
        cx.simulate_keystrokes("enter down enter");
        cx.run_until_parked();
        assert_eq!(staged(&mut cx, 0, 1), Some(None));

        // Up, then Esc: nothing changes.
        cx.simulate_keystrokes("enter up escape");
        cx.run_until_parked();
        assert_eq!(editing(&mut cx), None);
        assert_eq!(staged(&mut cx, 0, 1), Some(None));

        // Up twice from NULL, Tab saves and moves on to the text column,
        // which still edits as text (value selected, so typing replaces it).
        cx.simulate_keystrokes("enter up up tab");
        cx.run_until_parked();
        assert_eq!(staged(&mut cx, 0, 1), Some(Some("ok".into())));
        assert_eq!(editing(&mut cx), Some((0, 2)));
        cx.simulate_input("hi");
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        assert_eq!(staged(&mut cx, 0, 2), Some(Some("hi".into())));

        sql("drop table it_ui_enum; drop type it_ui_mood");
    }

    /// Editing query results, TablePlus-style: a single-table SELECT with its
    /// primary key is editable and ⌘S saves to the table; others are read-only.
    #[gpui::test]
    fn query_results_are_editable(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let mut cfg = ConnectionConfig::new(Driver::Sqlite);
        let file = std::env::temp_dir().join(format!("tabula-ui-results-{}.db", cfg.id));
        std::fs::write(&file, b"").unwrap();
        cfg.file = file.to_string_lossy().into();
        let setup = cfg.clone();
        let db = cfg.clone();
        block_on(crate::runtime::spawn(async move {
            let c = crate::db::Connection::connect(&setup).await?;
            c.execute(
                "create table t (id integer primary key, a text); \
                 create table u (t_id int, b text); \
                 insert into t (a) values ('x'), ('y')",
            )
            .await?;
            c.close().await;
            Ok(())
        }))
        .unwrap();

        let (app, mut cx) = open(cx);
        app.update_in(&mut cx, |a, w, cx| a.connect(cfg, w, cx, |_, _, _| {}));
        wait_until(&mut cx, "connection", |cx| {
            app.read_with(cx, |a, _| a.sessions.len() == 1)
        });
        let session = app.read_with(&cx, |a, _| a.sessions[0].clone());
        session.update_in(&mut cx, |s, w, cx| s.new_query_for_test(w, cx));
        let q = session.read_with(&cx, |s, _| s.active_query().unwrap());
        let run = |cx: &mut VisualTestContext, sql: &'static str| {
            q.update_in(cx, |q, w, cx| q.run_for_test(sql, w, cx));
            wait_until(cx, "query", |cx| q.read_with(cx, |q, _| !q.running()));
        };

        // Read-only cases say why.
        run(&mut cx, "select t.a, u.b from t join u on u.t_id = t.id");
        assert!(q.read_with(&cx, |q, _| q.note().unwrap().contains("single-table")));
        run(&mut cx, "select a from t");
        wait_until(&mut cx, "note", |cx| {
            q.read_with(cx, |q, _| q.note().is_some())
        });
        assert!(q.read_with(&cx, |q, _| q.note().unwrap().contains("primary key (id)")));

        // Editable: type into a cell, Enter, ⌘S writes to the table.
        run(&mut cx, "select * from t where id <= 2 order by id");
        wait_until(&mut cx, "editable", |cx| {
            q.read_with(cx, |q, _| q.is_editable())
        });
        q.update_in(&mut cx, |q, w, cx| q.select_cell(1, 1, w, cx));
        cx.simulate_keystrokes("z enter");
        cx.run_until_parked();
        assert!(q.read_with(&cx, |q, cx| q.is_dirty(cx)), "edit staged");
        cx.simulate_keystrokes("cmd-s");
        wait_until(&mut cx, "commit + re-run", |cx| {
            q.read_with(cx, |q, cx| {
                !q.is_dirty(cx) && !q.running() && q.cell(1, 1, cx).as_deref() == Some("z")
            })
        });
        let saved = block_on(crate::runtime::spawn(async move {
            let c = crate::db::Connection::connect(&db).await?;
            let r = c.execute("select a from t where id = 2").await?;
            Ok(r[0].rows[0][0].clone())
        }))
        .unwrap();
        assert_eq!(saved.as_deref(), Some("z"), "written to the database");
        let _ = std::fs::remove_file(file);
    }

    /// Runs are saved; ⌘Y lists them (searchable) and Enter reopens one in
    /// a new query tab. ⌘. stops a running query.
    #[gpui::test]
    fn query_history_and_cancel(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let mut cfg = ConnectionConfig::new(Driver::Sqlite);
        let file = std::env::temp_dir().join(format!("tabula-ui-history-{}.db", cfg.id));
        std::fs::write(&file, b"").unwrap();
        cfg.file = file.to_string_lossy().into();
        let (app, mut cx) = open(cx);
        app.update_in(&mut cx, |a, w, cx| a.connect(cfg, w, cx, |_, _, _| {}));
        wait_until(&mut cx, "connection", |cx| {
            app.read_with(cx, |a, _| a.sessions.len() == 1)
        });
        let session = app.read_with(&cx, |a, _| a.sessions[0].clone());
        session.update_in(&mut cx, |s, w, cx| s.new_query_for_test(w, cx));
        let q = session.read_with(&cx, |s, _| s.active_query().unwrap());
        for sql in ["select 'apple' as fruit", "select 'banana' as fruit"] {
            q.update_in(&mut cx, |q, w, cx| q.run_for_test(sql, w, cx));
            wait_until(&mut cx, "query", |cx| q.read_with(cx, |q, _| !q.running()));
        }

        // ⌘. while a slow query runs stops it.
        q.update_in(&mut cx, |q, w, cx| {
            q.run_for_test(
                "with recursive n(x) as (select 1 union all select x + 1 from n \
                 where x < 1000000000) select count(*) from n",
                w,
                cx,
            )
        });
        assert!(q.read_with(&cx, |q, _| q.running()));
        // Past the skeleton's delay: the chip with the clock and Stop shows.
        std::thread::sleep(Duration::from_millis(200));
        cx.executor().advance_clock(Duration::from_millis(200));
        cx.run_until_parked();
        assert!(cx.debug_bounds("loading-chip").is_some(), "running chip");
        cx.simulate_keystrokes("cmd-.");
        wait_until(&mut cx, "cancelled", |cx| {
            q.read_with(cx, |q, _| !q.running())
        });
        cx.run_until_parked();
        assert!(cx.debug_bounds("loading-chip").is_none(), "chip gone");

        let tabs = session.read_with(&cx, |s, _| s.tab_count());
        cx.simulate_keystrokes("cmd-y");
        wait_until(&mut cx, "history picker", |cx| {
            cx.update(|w, cx| w.has_active_dialog(cx))
        });
        cx.simulate_input("apple");
        cx.simulate_keystrokes("enter");
        wait_until(&mut cx, "reopened", |cx| {
            session.read_with(cx, |s, _| s.tab_count() == tabs + 1)
        });
        let reopened = session.read_with(&cx, |s, _| s.active_query().unwrap());
        assert_eq!(
            reopened.read_with(&cx, |q, cx| q.text(cx)),
            "select 'apple' as fruit"
        );
        assert!(!cx.update(|w, cx| w.has_active_dialog(cx)));

        // Insert at cursor puts a past query into the open tab instead.
        cx.simulate_keystrokes("cmd-down enter");
        cx.simulate_keystrokes("cmd-y");
        wait_until(&mut cx, "history again", |cx| {
            cx.update(|w, cx| w.has_active_dialog(cx))
        });
        cx.simulate_input("banana");
        cx.run_until_parked();
        let b = cx.debug_bounds("history-insert").expect("insert button");
        cx.simulate_click(b.center(), Modifiers::none());
        cx.run_until_parked();
        assert_eq!(session.read_with(&cx, |s, _| s.tab_count()), tabs + 1);
        assert_eq!(
            reopened.read_with(&cx, |q, cx| q.text(cx)),
            "select 'apple' as fruit\nselect 'banana' as fruit"
        );
        let _ = std::fs::remove_file(file);
    }

    /// A foreign-key cell's → opens the parent table filtered to that row;
    /// the chip's ✕ shows every row again.
    #[gpui::test]
    fn follow_foreign_key(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let mut cfg = ConnectionConfig::new(Driver::Sqlite);
        let file = std::env::temp_dir().join(format!("tabula-ui-fk-{}.db", cfg.id));
        std::fs::write(&file, b"").unwrap();
        cfg.file = file.to_string_lossy().into();
        let setup = cfg.clone();
        block_on(crate::runtime::spawn(async move {
            let c = crate::db::Connection::connect(&setup).await?;
            c.execute(
                "create table author (id integer primary key, name text); \
                 create table book (id integer primary key, title text, \
                   author_id int references author (id)); \
                 insert into author (name) values ('Ada'), ('Grace'), ('Linus'); \
                 insert into book (title, author_id) values ('Notes', 1), ('COBOL', 2), ('Anon', null)",
            )
            .await?;
            c.close().await;
            Ok(())
        }))
        .unwrap();

        let (app, mut cx) = open(cx);
        app.update_in(&mut cx, |a, w, cx| a.connect(cfg, w, cx, |_, _, _| {}));
        wait_until(&mut cx, "connection", |cx| {
            app.read_with(cx, |a, _| a.sessions.len() == 1)
        });
        let session = app.read_with(&cx, |a, _| a.sessions[0].clone());
        wait_until(&mut cx, "tables", |cx| {
            session.read_with(cx, |s, _| s.object_index("book").is_some())
        });
        session.update_in(&mut cx, |s, w, cx| {
            let i = s.object_index("book").unwrap();
            s.open_object(i, w, cx)
        });
        let book = session.read_with(&cx, |s, _| s.active_table().unwrap());
        wait_until(&mut cx, "rows", |cx| {
            book.read_with(cx, |t, cx| t.loaded_rows(cx) == 3)
        });
        assert_eq!(
            book.read_with(&cx, |t, cx| t.links(cx)),
            [false, false, true]
        );

        // Click the → in book row 2 ("COBOL", author 2).
        cx.run_until_parked();
        let link = cx
            .debug_bounds("fk-link-1-2")
            .expect("link drawn on a non-NULL foreign-key cell");
        assert!(cx.debug_bounds("fk-link-2-2").is_none(), "no link on NULL");
        cx.simulate_click(link.center(), Modifiers::none());
        wait_until(&mut cx, "author tab", |cx| {
            session.read_with(cx, |s, cx| {
                s.active_table()
                    .is_some_and(|t| t.read(cx).object.name == "author")
            })
        });
        let author = session.read_with(&cx, |s, _| s.active_table().unwrap());
        wait_until(&mut cx, "filtered", |cx| {
            author.read_with(cx, |t, cx| t.loaded_rows(cx) == 1)
        });
        assert_eq!(
            author.read_with(&cx, |t, cx| t.cell(0, 1, cx)).as_deref(),
            Some("Grace")
        );

        // The chip's ✕ clears the reference filter.
        cx.run_until_parked();
        let clear = cx.debug_bounds("clear-fixed").expect("chip shown");
        cx.simulate_click(clear.center(), Modifiers::none());
        wait_until(&mut cx, "all authors", |cx| {
            author.read_with(cx, |t, cx| t.loaded_rows(cx) == 3)
        });
        let _ = std::fs::remove_file(file);
    }

    /// ⌘⇧I shows the selected row's fields; a field's pencil edits it.
    #[gpui::test]
    fn row_detail_panel(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let mut cfg = ConnectionConfig::new(Driver::Sqlite);
        let file = std::env::temp_dir().join(format!("tabula-ui-detail-{}.db", cfg.id));
        std::fs::write(&file, b"").unwrap();
        cfg.file = file.to_string_lossy().into();
        let setup = cfg.clone();
        block_on(crate::runtime::spawn(async move {
            let c = crate::db::Connection::connect(&setup).await?;
            c.execute(
                "create table t (id integer primary key, doc text); \
                 insert into t (doc) values ('{\"a\":1}'), ('plain')",
            )
            .await?;
            c.close().await;
            Ok(())
        }))
        .unwrap();
        let (app, mut cx) = open(cx);
        app.update_in(&mut cx, |a, w, cx| a.connect(cfg, w, cx, |_, _, _| {}));
        wait_until(&mut cx, "connection", |cx| {
            app.read_with(cx, |a, _| a.sessions.len() == 1)
        });
        let session = app.read_with(&cx, |a, _| a.sessions[0].clone());
        wait_until(&mut cx, "tables", |cx| {
            session.read_with(cx, |s, _| s.object_index("t").is_some())
        });
        session.update_in(&mut cx, |s, w, cx| {
            let i = s.object_index("t").unwrap();
            s.open_object(i, w, cx)
        });
        let tab = session.read_with(&cx, |s, _| s.active_table().unwrap());
        wait_until(&mut cx, "rows", |cx| {
            tab.read_with(cx, |t, cx| t.loaded_rows(cx) == 2)
        });
        tab.update_in(&mut cx, |t, w, cx| t.select_cell(1, 0, w, cx));
        cx.simulate_keystrokes("cmd-shift-i");
        cx.run_until_parked();
        assert!(tab.read_with(&cx, |t, _| t.show_detail));
        assert!(cx.debug_bounds("row-detail").is_some(), "panel shown");

        let edit = cx.debug_bounds("detail-edit-1").expect("doc is editable");
        cx.simulate_click(edit.center(), Modifiers::none());
        cx.run_until_parked();
        assert_eq!(tab.read_with(&cx, |t, cx| t.editing_cell(cx)), Some((1, 1)));
        cx.simulate_input("changed");
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        assert_eq!(
            tab.read_with(&cx, |t, cx| t.staged(1, 1, cx)),
            Some(Some("changed".into()))
        );
        cx.simulate_keystrokes("cmd-shift-i");
        cx.run_until_parked();
        assert!(cx.debug_bounds("row-detail").is_none(), "panel hidden");
        let _ = std::fs::remove_file(file);
    }

    /// ⌘⇧F formats the editor while typing in it; ⌘Z undoes it.
    #[gpui::test]
    fn format_sql_shortcut(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let mut cfg = ConnectionConfig::new(Driver::Sqlite);
        let file = std::env::temp_dir().join(format!("tabula-ui-format-{}.db", cfg.id));
        std::fs::write(&file, b"").unwrap();
        cfg.file = file.to_string_lossy().into();
        let (app, mut cx) = open(cx);
        app.update_in(&mut cx, |a, w, cx| a.connect(cfg, w, cx, |_, _, _| {}));
        wait_until(&mut cx, "connection", |cx| {
            app.read_with(cx, |a, _| a.sessions.len() == 1)
        });
        let session = app.read_with(&cx, |a, _| a.sessions[0].clone());
        session.update_in(&mut cx, |s, w, cx| s.new_query_for_test(w, cx));
        let q = session.read_with(&cx, |s, _| s.active_query().unwrap());
        cx.run_until_parked();
        cx.simulate_input("select a, b from t where a = 1");
        cx.simulate_keystrokes("cmd-shift-f");
        cx.run_until_parked();
        assert_eq!(
            q.read_with(&cx, |q, cx| q.text(cx)),
            "SELECT\n  a,\n  b\nFROM t\nWHERE\n  a = 1"
        );
        cx.simulate_keystrokes("cmd-z");
        cx.run_until_parked();
        assert_eq!(
            q.read_with(&cx, |q, cx| q.text(cx)),
            "select a, b from t where a = 1"
        );

        // ⌘E shows the plan of the editor's statement.
        q.update_in(&mut cx, |q, w, cx| {
            q.run_for_test("create table t (a int, b int)", w, cx)
        });
        wait_until(&mut cx, "table", |cx| q.read_with(cx, |q, _| !q.running()));
        cx.simulate_keystrokes("cmd-e");
        wait_until(&mut cx, "plan", |cx| {
            q.read_with(cx, |q, _| !q.plan_labels().is_empty())
        });
        assert!(q.read_with(&cx, |q, _| {
            q.plan_labels().iter().any(|l| l.contains("SCAN t"))
        }));
        let _ = std::fs::remove_file(file);
    }

    /// ⌘⇧S names and saves the editor's query; ⌘⇧O reopens it by name;
    /// ⌘⇧S in that tab updates it in place.
    #[gpui::test]
    fn saved_queries(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let mut cfg = ConnectionConfig::new(Driver::Sqlite);
        let file = std::env::temp_dir().join(format!("tabula-ui-saved-{}.db", cfg.id));
        std::fs::write(&file, b"").unwrap();
        cfg.file = file.to_string_lossy().into();
        let conn_id = cfg.id.clone();
        let (app, mut cx) = open(cx);
        app.update_in(&mut cx, |a, w, cx| a.connect(cfg, w, cx, |_, _, _| {}));
        wait_until(&mut cx, "connection", |cx| {
            app.read_with(cx, |a, _| a.sessions.len() == 1)
        });
        let session = app.read_with(&cx, |a, _| a.sessions[0].clone());
        session.update_in(&mut cx, |s, w, cx| s.new_query_for_test(w, cx));
        cx.run_until_parked();
        cx.simulate_input("select 'kept'");
        cx.simulate_keystrokes("cmd-shift-s");
        cx.run_until_parked();
        assert!(cx.update(|w, cx| w.has_active_dialog(cx)), "name dialog");
        cx.simulate_input("Kept query");
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        let saved = crate::saved::for_connection(&conn_id).unwrap();
        let mine: Vec<_> = saved
            .iter()
            .filter(|q| q.conn_id.as_deref() == Some(&conn_id))
            .collect();
        assert_eq!(mine.len(), 1);
        assert_eq!(
            (mine[0].name.as_str(), mine[0].sql.as_str()),
            ("Kept query", "select 'kept'")
        );

        let tabs = session.read_with(&cx, |s, _| s.tab_count());
        cx.simulate_keystrokes("cmd-shift-o");
        cx.run_until_parked();
        cx.simulate_input("Kept");
        cx.simulate_keystrokes("enter");
        wait_until(&mut cx, "opened", |cx| {
            session.read_with(cx, |s, _| s.tab_count() == tabs + 1)
        });
        let q = session.read_with(&cx, |s, _| s.active_query().unwrap());
        assert_eq!(q.read_with(&cx, |q, _| q.title.clone()), "Kept query");
        assert_eq!(q.read_with(&cx, |q, cx| q.text(cx)), "select 'kept'");

        // Editing and saving again updates the same entry, no dialog.
        cx.simulate_keystrokes("cmd-down");
        cx.simulate_input(" -- v2");
        cx.simulate_keystrokes("cmd-shift-s");
        cx.run_until_parked();
        assert!(!cx.update(|w, cx| w.has_active_dialog(cx)));
        let again = crate::saved::for_connection(&conn_id).unwrap();
        let mine: Vec<_> = again
            .iter()
            .filter(|q| q.conn_id.as_deref() == Some(&conn_id))
            .collect();
        assert_eq!(mine.len(), 1);
        assert_eq!(mine[0].sql, "select 'kept' -- v2");
        crate::saved::delete(&mine[0].id).unwrap();
        let _ = std::fs::remove_file(file);
    }

    /// Import CSV: rows go in after confirming; a bad row inserts nothing.
    #[gpui::test]
    fn csv_import(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let mut cfg = ConnectionConfig::new(Driver::Sqlite);
        let file = std::env::temp_dir().join(format!("tabula-ui-csv-{}.db", cfg.id));
        std::fs::write(&file, b"").unwrap();
        cfg.file = file.to_string_lossy().into();
        let setup = cfg.clone();
        block_on(crate::runtime::spawn(async move {
            let c = crate::db::Connection::connect(&setup).await?;
            c.execute("create table people (id integer primary key, name text not null, age int)")
                .await?;
            c.close().await;
            Ok(())
        }))
        .unwrap();
        let good = std::env::temp_dir().join(format!("tabula-ui-good-{}.csv", cfg.id));
        std::fs::write(
            &good,
            "Name,Age,Unknown\nAda,36,x\n\"Hopper, Grace\",85,y\nLinus,,z\n",
        )
        .unwrap();
        let bad = std::env::temp_dir().join(format!("tabula-ui-bad-{}.csv", cfg.id));
        std::fs::write(&bad, "name,age\nKen,1\n,2\n").unwrap();

        let (app, mut cx) = open(cx);
        app.update_in(&mut cx, |a, w, cx| a.connect(cfg, w, cx, |_, _, _| {}));
        wait_until(&mut cx, "connection", |cx| {
            app.read_with(cx, |a, _| a.sessions.len() == 1)
        });
        let session = app.read_with(&cx, |a, _| a.sessions[0].clone());
        wait_until(&mut cx, "tables", |cx| {
            session.read_with(cx, |s, _| s.object_index("people").is_some())
        });
        session.update_in(&mut cx, |s, w, cx| {
            let i = s.object_index("people").unwrap();
            s.open_object(i, w, cx)
        });
        let tab = session.read_with(&cx, |s, _| s.active_table().unwrap());
        wait_until(&mut cx, "structure", |cx| {
            tab.read_with(cx, |t, _| t.has_structure())
        });

        // Waits for the import dialog of `file` and presses Import.
        let import = |cx: &mut VisualTestContext, file: &Path| {
            let before = tab.read_with(cx, |t, _| t.import_view_for_test().map(|v| v.entity_id()));
            tab.update_in(cx, |t, w, cx| t.import_csv_file(file, w, cx));
            wait_until(cx, "import dialog", |cx| {
                tab.read_with(cx, |t, _| t.import_view_for_test().map(|v| v.entity_id())) != before
            });
            tab.read_with(cx, |t, _| t.import_view_for_test().unwrap())
        };
        let dialog = import(&mut cx, &good);
        dialog.update_in(&mut cx, |d, w, cx| d.import_for_test(w, cx));
        wait_until(&mut cx, "imported", |cx| {
            tab.read_with(cx, |t, cx| t.loaded_rows(cx) == 3)
        });
        assert_eq!(
            tab.read_with(&cx, |t, cx| t.cell(1, 1, cx)).as_deref(),
            Some("Hopper, Grace")
        );
        assert_eq!(
            tab.read_with(&cx, |t, cx| t.cell(2, 2, cx)),
            None,
            "empty → NULL"
        );

        // The second row has no name (NOT NULL): the whole file is rejected.
        cx.executor().advance_clock(Duration::from_secs(1));
        let dialog = import(&mut cx, &bad);
        dialog.update_in(&mut cx, |d, w, cx| d.import_for_test(w, cx));
        cx.run_until_parked();
        std::thread::sleep(std::time::Duration::from_millis(200));
        cx.run_until_parked();
        assert_eq!(
            tab.read_with(&cx, |t, cx| t.loaded_rows(cx)),
            3,
            "nothing inserted"
        );

        // An age that isn't a number fails validation: Import waits until
        // those rows are skipped, then the rest go in.
        let typed = good.with_extension("typed.csv");
        write(&typed, "name,age\nKen,abc\nBarbara,79\n").unwrap();
        cx.executor().advance_clock(Duration::from_secs(1));
        let dialog = import(&mut cx, &typed);
        dialog.update_in(&mut cx, |d, w, cx| d.import_for_test(w, cx));
        cx.run_until_parked();
        assert!(
            cx.update(|w, cx| w.has_active_dialog(cx)),
            "blocked by the bad row"
        );
        dialog.update_in(&mut cx, |d, w, cx| {
            d.skip_invalid_for_test(w, cx);
            d.import_for_test(w, cx);
        });
        wait_until(&mut cx, "valid row imported", |cx| {
            tab.read_with(cx, |t, cx| t.loaded_rows(cx) == 4)
        });
        let _ = remove_file(typed);
        for f in [file, good, bad] {
            let _ = std::fs::remove_file(f);
        }
    }

    /// New Table creates it (sidebar updates); Structure › Columns adds
    /// and drops columns in place on the open tab.
    #[gpui::test]
    fn table_editor(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let mut cfg = ConnectionConfig::new(Driver::Sqlite);
        let file = std::env::temp_dir().join(format!("tabula-ui-ddl-{}.db", cfg.id));
        std::fs::write(&file, b"").unwrap();
        cfg.file = file.to_string_lossy().into();
        let (app, mut cx) = open(cx);
        app.update_in(&mut cx, |a, w, cx| a.connect(cfg, w, cx, |_, _, _| {}));
        wait_until(&mut cx, "connection", |cx| {
            app.read_with(cx, |a, _| a.sessions.len() == 1)
        });
        let session = app.read_with(&cx, |a, _| a.sessions[0].clone());
        let ed = session.update_in(&mut cx, |s, w, cx| s.new_table_for_test(w, cx));
        ed.update_in(&mut cx, |e, w, cx| {
            e.set_name("books", w, cx);
            e.set_row(1, "title", "text", w, cx);
            e.set_row(2, "legacy", "int", w, cx);
            e.apply_for_test(w, cx);
        });
        wait_until(&mut cx, "created", |cx| {
            session.read_with(cx, |s, _| s.object_index("books").is_some())
        });
        assert!(!cx.update(|w, cx| w.has_active_dialog(cx)), "dialog closed");

        session.update_in(&mut cx, |s, w, cx| {
            let i = s.object_index("books").unwrap();
            s.open_object(i, w, cx)
        });
        let tab = session.read_with(&cx, |s, _| s.active_table().unwrap());
        wait_until(&mut cx, "structure", |cx| {
            tab.read_with(cx, |t, _| t.has_structure())
        });
        assert_eq!(
            tab.read_with(&cx, |t, _| t.column_names()),
            ["id", "title", "legacy"]
        );
        // Structure › Columns edits in place: stage, discard, stage, apply.
        let columns = tab
            .update_in(&mut cx, |t, w, cx| t.columns_editor_for_test(w, cx))
            .unwrap();
        columns.update_in(&mut cx, |editor, w, cx| {
            editor.remove_row_for_test(2);
            editor.set_row_for_test(3, "year", "integer", w, cx);
        });
        assert_eq!(columns.read_with(&cx, |e, cx| e.change_count(cx)), 2);
        columns.update_in(&mut cx, |editor, w, cx| editor.discard_for_test(w, cx));
        assert_eq!(columns.read_with(&cx, |e, cx| e.change_count(cx)), 0);
        columns.update_in(&mut cx, |editor, w, cx| {
            editor.remove_row_for_test(2);
            editor.set_row_for_test(3, "year", "integer", w, cx);
            editor.apply_for_test(w, cx);
        });
        wait_until(&mut cx, "altered", |cx| {
            tab.read_with(cx, |t, _| t.column_names() == ["id", "title", "year"])
        });
        cx.run_until_parked();

        // Start from "id + timestamps", and "Copy of…" another table.
        let ed = session.update_in(&mut cx, |s, w, cx| s.new_table_for_test(w, cx));
        ed.update_in(&mut cx, |e, w, cx| {
            e.set_name("events", w, cx);
            e.start_for_test(None, w, cx);
        });
        assert_eq!(ed.read_with(&cx, |e, _| e.column_count()), 3);
        ed.update_in(&mut cx, |e, w, cx| e.apply_for_test(w, cx));
        wait_until(&mut cx, "events created", |cx| {
            session.read_with(cx, |s, _| s.object_index("events").is_some())
        });
        cx.executor().advance_clock(Duration::from_secs(1));
        cx.run_until_parked();
        let ed = session.update_in(&mut cx, |s, w, cx| s.new_table_for_test(w, cx));
        ed.update_in(&mut cx, |e, w, cx| e.start_for_test(Some("books"), w, cx));
        wait_until(&mut cx, "copied columns", |cx| {
            ed.read_with(cx, |e, _| e.column_count() == 3)
        });
        let _ = std::fs::remove_file(file);
    }

    /// + Index creates an index (named after its columns); Drop Index…
    /// confirms, then drops it.
    #[gpui::test]
    fn create_and_drop_index(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let mut cfg = ConnectionConfig::new(Driver::Sqlite);
        let file = temp_dir().join(format!("tabula-ui-index-{}.db", cfg.id));
        write(&file, b"").unwrap();
        cfg.file = file.to_string_lossy().into();
        let setup = cfg.clone();
        block_on(crate::runtime::spawn(async move {
            let c = crate::db::Connection::connect(&setup).await?;
            c.execute("create table t (id integer primary key, status text)")
                .await?;
            c.close().await;
            Ok(())
        }))
        .unwrap();
        let (app, mut cx) = open(cx);
        app.update_in(&mut cx, |a, w, cx| a.connect(cfg, w, cx, |_, _, _| {}));
        wait_until(&mut cx, "connection", |cx| {
            app.read_with(cx, |a, _| a.sessions.len() == 1)
        });
        let session = app.read_with(&cx, |a, _| a.sessions[0].clone());
        wait_until(&mut cx, "tables", |cx| {
            session.read_with(cx, |s, _| s.object_index("t").is_some())
        });
        session.update_in(&mut cx, |s, w, cx| {
            let index = s.object_index("t").unwrap();
            s.open_object(index, w, cx)
        });
        let tab = session.read_with(&cx, |s, _| s.active_table().unwrap());
        wait_until(&mut cx, "structure", |cx| {
            tab.read_with(cx, |t, _| t.has_structure())
        });
        let index_dialog = tab
            .update_in(&mut cx, |t, w, cx| t.open_index_dialog(w, cx))
            .unwrap();
        index_dialog.update_in(&mut cx, |dialog, w, cx| {
            dialog.toggle_column("status", w, cx);
            dialog.create(w, cx);
        });
        wait_until(&mut cx, "index created", |cx| {
            tab.read_with(cx, |t, _| {
                t.index_names().contains(&"t_status_idx".to_string())
            })
        });
        cx.executor().advance_clock(Duration::from_secs(1));
        cx.run_until_parked();
        assert!(
            !cx.update(|w, cx| w.has_active_dialog(cx)),
            "index dialog closed"
        );
        tab.update_in(&mut cx, |t, w, cx| {
            t.drop_index_for_test("t_status_idx", w, cx)
        });
        cx.run_until_parked();
        assert!(
            cx.update(|w, cx| w.has_active_dialog(cx)),
            "drop asks first"
        );
        // ↵ confirms away from production.
        cx.simulate_keystrokes("enter");
        wait_until(&mut cx, "index dropped", |cx| {
            tab.read_with(cx, |t, _| {
                !t.index_names().contains(&"t_status_idx".to_string())
            })
        });
        let _ = remove_file(file);
    }

    /// Rename… asks for the new name (prefilled, selected) and renames.
    #[gpui::test]
    fn rename_table(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let mut cfg = ConnectionConfig::new(Driver::Sqlite);
        let file = std::env::temp_dir().join(format!("tabula-ui-rename-{}.db", cfg.id));
        std::fs::write(&file, b"").unwrap();
        cfg.file = file.to_string_lossy().into();
        let setup = cfg.clone();
        block_on(crate::runtime::spawn(async move {
            let c = crate::db::Connection::connect(&setup).await?;
            c.execute("create table old_name (id integer primary key)")
                .await?;
            c.close().await;
            Ok(())
        }))
        .unwrap();
        let (app, mut cx) = open(cx);
        app.update_in(&mut cx, |a, w, cx| a.connect(cfg, w, cx, |_, _, _| {}));
        wait_until(&mut cx, "connection", |cx| {
            app.read_with(cx, |a, _| a.sessions.len() == 1)
        });
        let session = app.read_with(&cx, |a, _| a.sessions[0].clone());
        wait_until(&mut cx, "tables", |cx| {
            session.read_with(cx, |s, _| s.object("old_name").is_some())
        });
        let obj = session.read_with(&cx, |s, _| s.object("old_name").unwrap());
        session.update_in(&mut cx, |s, w, cx| s.rename_object(obj, w, cx));
        cx.run_until_parked();
        cx.simulate_input("new_name");
        cx.simulate_keystrokes("enter");
        wait_until(&mut cx, "renamed", |cx| {
            session.read_with(cx, |s, _| {
                s.object("new_name").is_some() && s.object("old_name").is_none()
            })
        });
        assert!(!cx.update(|w, cx| w.has_active_dialog(cx)));
        let _ = std::fs::remove_file(file);
    }

    /// Tabs (with unsaved query text) survive a restart: snapshot, close,
    /// reopen the connection with the saved state.
    #[gpui::test]
    fn session_restore(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let mut cfg = ConnectionConfig::new(Driver::Sqlite);
        let file = std::env::temp_dir().join(format!("tabula-ui-restore-{}.db", cfg.id));
        std::fs::write(&file, b"").unwrap();
        cfg.file = file.to_string_lossy().into();
        let setup = cfg.clone();
        block_on(crate::runtime::spawn(async move {
            let c = crate::db::Connection::connect(&setup).await?;
            c.execute("create table kept (id integer primary key)")
                .await?;
            c.close().await;
            Ok(())
        }))
        .unwrap();
        let (app, mut cx) = open(cx);
        app.update_in(&mut cx, |a, w, cx| {
            a.connect(cfg.clone(), w, cx, |_, _, _| {})
        });
        wait_until(&mut cx, "connection", |cx| {
            app.read_with(cx, |a, _| a.sessions.len() == 1)
        });
        let session = app.read_with(&cx, |a, _| a.sessions[0].clone());
        wait_until(&mut cx, "tables", |cx| {
            session.read_with(cx, |s, _| s.object_index("kept").is_some())
        });
        session.update_in(&mut cx, |s, w, cx| {
            let i = s.object_index("kept").unwrap();
            s.open_object(i, w, cx);
            s.new_query_for_test(w, cx);
        });
        cx.run_until_parked();
        cx.simulate_input("select 'draft'");
        let state = app.read_with(&cx, |a, cx| a.snapshot_for_test(cx));
        assert_eq!(state.sessions[0].tabs.len(), 2);

        app.update_in(&mut cx, |a, _, cx| a.remove_session(0, cx));
        app.update_in(&mut cx, |a, w, cx| {
            a.reopen_for_test(state.sessions[0].clone(), cfg, w, cx)
        });
        wait_until(&mut cx, "reopened", |cx| {
            app.read_with(cx, |a, _| a.sessions.len() == 1)
        });
        let s = app.read_with(&cx, |a, _| a.sessions[0].clone());
        assert_eq!(s.read_with(&cx, |s, _| s.tab_count()), 2);
        let q = s.read_with(&cx, |s, _| s.active_query().expect("query tab in front"));
        assert_eq!(q.read_with(&cx, |q, cx| q.text(cx)), "select 'draft'");
        let _ = std::fs::remove_file(file);
    }

    /// Closing a tab left of the visible one keeps showing the same tab.
    #[gpui::test]
    fn closing_a_tab_keeps_the_visible_one(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let mut cfg = ConnectionConfig::new(Driver::Sqlite);
        let file = std::env::temp_dir().join(format!("tabula-ui-tabs-{}.db", cfg.id));
        std::fs::write(&file, b"").unwrap();
        cfg.file = file.to_string_lossy().into();
        let (app, mut cx) = open(cx);
        app.update_in(&mut cx, |a, w, cx| a.connect(cfg, w, cx, |_, _, _| {}));
        wait_until(&mut cx, "connection", |cx| {
            app.read_with(cx, |a, _| a.sessions.len() == 1)
        });
        let session = app.read_with(&cx, |a, _| a.sessions[0].clone());
        for _ in 0..3 {
            session.update_in(&mut cx, |s, w, cx| s.new_query_for_test(w, cx));
        }
        let shown = session.read_with(&cx, |s, _| s.active_query().unwrap());
        session.update_in(&mut cx, |s, w, cx| s.close_tab_for_test(0, w, cx));
        let now = session.read_with(&cx, |s, _| s.active_query().unwrap());
        assert_eq!(now.entity_id(), shown.entity_id());
        let _ = std::fs::remove_file(file);
    }

    /// The server stops answering: the sidebar keeps an error with Retry
    /// (not a stale or silently empty list); Retry recovers once it's back.
    #[gpui::test]
    fn sidebar_shows_load_errors(cx: &mut TestAppContext) {
        if std::env::var("TABULA_IT_PAUSE").is_err() {
            return;
        }
        cx.executor().allow_parking();
        let mut cfg = ConnectionConfig::new(Driver::Postgres);
        cfg.host = "127.0.0.1".into();
        cfg.port = 55432;
        cfg.user = "postgres".into();
        cfg.password = "test".into();
        cfg.database = "postgres".into();
        let (app, mut cx) = open(cx);
        app.update_in(&mut cx, |a, w, cx| a.connect(cfg, w, cx, |_, _, _| {}));
        wait_until(&mut cx, "connection", |cx| {
            app.read_with(cx, |a, _| a.sessions.len() == 1)
        });
        let session = app.read_with(&cx, |a, _| a.sessions[0].clone());
        cx.run_until_parked();
        std::process::Command::new("docker")
            .args(["pause", "tabula-test-pg"])
            .status()
            .unwrap();
        session.update_in(&mut cx, |s, w, cx| s.refresh(w, cx));
        let start = Instant::now();
        while session.read_with(&cx, |s, _| s.load_error().is_none()) {
            assert!(start.elapsed() < Duration::from_secs(45), "error shown");
            std::thread::sleep(Duration::from_millis(100));
            cx.run_until_parked();
        }
        std::process::Command::new("docker")
            .args(["unpause", "tabula-test-pg"])
            .status()
            .unwrap();
        let e = session.read_with(&cx, |s, _| s.load_error().unwrap());
        assert!(
            e.contains("Lost the connection") || e.contains("didn't answer"),
            "{e}"
        );
        cx.run_until_parked();
        let retry = cx.debug_bounds("retry-objects").expect("Retry shown");
        cx.simulate_click(retry.center(), Modifiers::none());
        wait_until(&mut cx, "recovered", |cx| {
            session.read_with(cx, |s, _| s.load_error().is_none())
        });
    }

    /// ⌘1/⌘2 switch connections; ⌘N opens the connection picker, which
    /// switches to an open connection or offers "New Connection…".
    #[gpui::test]
    fn connection_switching(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let (app, mut cx) = open(cx);
        let mut files = vec![];
        for name in ["tabula-ui-alpha", "tabula-ui-beta"] {
            let mut cfg = ConnectionConfig::new(Driver::Sqlite);
            cfg.name = name.into();
            let file = std::env::temp_dir().join(format!("{name}-{}.db", cfg.id));
            std::fs::write(&file, b"").unwrap();
            cfg.file = file.to_string_lossy().into();
            files.push(file);
            app.update_in(&mut cx, |a, w, cx| a.connect(cfg, w, cx, |_, _, _| {}));
        }
        wait_until(&mut cx, "two connections", |cx| {
            app.read_with(cx, |a, _| a.sessions.len() == 2)
        });
        let active = |cx: &mut VisualTestContext| app.read_with(cx, |a, _| a.active);

        cx.update(|window, cx| window.blur(cx));
        cx.simulate_keystrokes("cmd-1");
        cx.run_until_parked();
        assert_eq!(active(&mut cx), Some(0));
        cx.simulate_keystrokes("cmd-2");
        cx.run_until_parked();
        assert_eq!(active(&mut cx), Some(1));
        cx.simulate_keystrokes("cmd-9");
        cx.run_until_parked();
        assert_eq!(active(&mut cx), Some(1), "no 9th connection: stays put");

        // ⌘⇧K opens the same picker as ⌘N and the rail "+".
        cx.simulate_keystrokes("cmd-shift-k");
        cx.run_until_parked();
        assert!(
            cx.update(|w, cx| w.has_active_dialog(cx)),
            "⌘⇧K opens the picker"
        );
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        assert!(!cx.update(|w, cx| w.has_active_dialog(cx)));

        // ⌘N → picker; typing a name and Enter switches to it.
        cx.simulate_keystrokes("cmd-n");
        cx.run_until_parked();
        assert!(cx.update(|w, cx| w.has_active_dialog(cx)));
        cx.simulate_input("tabula-ui-alpha");
        cx.run_until_parked();
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        assert_eq!(active(&mut cx), Some(0));
        assert!(!cx.update(|w, cx| w.has_active_dialog(cx)));

        // "New Connection…" opens the form.
        cx.simulate_keystrokes("cmd-n");
        cx.run_until_parked();
        cx.simulate_input("New Connection");
        cx.run_until_parked();
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        assert!(
            cx.update(|w, cx| w.has_active_dialog(cx)),
            "connection form open"
        );
        for f in files {
            let _ = std::fs::remove_file(f);
        }
    }

    /// Picking a saved (not yet open) connection opens it. Uses the test
    /// config folder, never the user's real saved connections.
    #[gpui::test]
    fn picker_opens_saved_connection(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let mut cfg = ConnectionConfig::new(Driver::Sqlite);
        cfg.name = "tabula-ui-saved".into();
        let file = std::env::temp_dir().join(format!("tabula-ui-saved-{}.db", cfg.id));
        std::fs::write(&file, b"").unwrap();
        cfg.file = file.to_string_lossy().into();
        crate::store::save_all(&[cfg]).unwrap();

        let (app, mut cx) = open(cx);
        cx.update(|window, cx| window.blur(cx));
        cx.simulate_keystrokes("cmd-n");
        cx.run_until_parked();
        cx.simulate_input("tabula-ui-saved");
        cx.run_until_parked();
        cx.simulate_keystrokes("enter");
        wait_until(&mut cx, "saved connection opened", |cx| {
            app.read_with(cx, |a, _| a.sessions.len() == 1 && a.active == Some(0))
        });
        let _ = std::fs::remove_file(file);
    }
}
