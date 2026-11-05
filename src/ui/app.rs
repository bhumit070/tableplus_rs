//! Window root: title bar, the connection rail on the left (home + one icon
//! per open connection, no limit), and the active view.

use std::rc::Rc;
use std::sync::Arc;

use gpui::{
    AnyWindowHandle, App, Context, Entity, FocusHandle, Global, InteractiveElement, IntoElement,
    ParentElement, Render, Styled, WeakEntity, Window, div, prelude::*, px,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::notification::Notification;
use gpui_component::tooltip::Tooltip;
use gpui_component::{
    ActiveTheme as _, Icon, Sizable as _, Theme, ThemeMode, TitleBar, WindowExt as _, h_flex,
    v_flex,
};
use gpui_kit_assets::IconName;
use tokio::sync::RwLock;

use super::home::Home;
use super::session::Session;
use super::settings_view::SettingsView;
use super::table_tab::TableTab;
use super::{
    AddRow, CloseTab, Commit, DeleteRows, Discard, GoHome, NewConnection, NewQuery, NextTab,
    OpenSettings, PrevTab, Refresh, RunQuery, Shared, SwitchDatabase, ToggleFilter, ToggleTheme,
    tag_color,
};
use crate::db::{Connection, ConnectionConfig};
use crate::runtime;
use crate::settings;

pub struct AppView {
    home: Entity<Home>,
    sessions: Vec<Entity<Session>>,
    /// `None` shows the connection list.
    active: Option<usize>,
    focus: FocusHandle,
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
        Self {
            home,
            sessions: Vec::new(),
            active: None,
            focus,
        }
    }

    /// Opens a connection in a new session. `done(ok)` runs either way.
    pub fn connect(
        &mut self,
        cfg: ConnectionConfig,
        window: &mut Window,
        cx: &mut Context<Self>,
        done: impl FnOnce(bool, &mut Window, &mut App) + 'static,
    ) {
        let c = cfg.clone();
        let task = runtime::spawn(async move { Connection::connect(&c).await });
        cx.spawn_in(window, async move |this, cx| {
            let res = task.await;
            this.update_in(cx, |this, window, cx| {
                done(res.is_ok(), window, cx);
                match res {
                    Ok(conn) => {
                        let shared = Rc::new(Shared {
                            cfg,
                            conn: Arc::new(RwLock::new(conn)),
                            schema: Default::default(),
                        });
                        let session = cx.new(|cx| Session::new(shared, window, cx));
                        this.sessions.push(session);
                        this.active = Some(this.sessions.len() - 1);
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
        let _ = runtime::spawn(async move {
            conn.read().await.close().await;
            Ok(())
        });
        self.active = match self.active {
            Some(a) if a == ix => {
                (!self.sessions.is_empty()).then(|| ix.min(self.sessions.len() - 1))
            }
            Some(a) if a > ix => Some(a - 1),
            other => other,
        };
        cx.notify();
    }

    fn rail_button(&self, ix: Option<usize>, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let selected = self.active == ix;
        let (label, color, tip) = match ix {
            None => (None, theme.primary, "Connections (⌘0)".to_string()),
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
                    tag_color(cfg),
                    format!("{} · {}", cfg.display_name(), cfg.driver.label()),
                )
            }
        };
        let id = ix.map_or(usize::MAX, |i| i);
        div()
            .id(("rail", id))
            .group("rail-item")
            .relative()
            .size(px(38.))
            .rounded_lg()
            .flex()
            .items_center()
            .justify_center()
            .cursor_pointer()
            .text_xs()
            .font_weight(gpui::FontWeight::BOLD)
            .border_2()
            .border_color(if selected {
                color
            } else {
                gpui::transparent_black()
            })
            .bg(if selected {
                theme.accent
            } else {
                theme.secondary
            })
            .hover(|s| s.bg(theme.accent))
            .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
            .map(|d| match &label {
                None => d.child(Icon::new(IconName::LayoutDashboard).small()),
                Some(l) => d.child(l.clone()).child(
                    div()
                        .absolute()
                        .bottom(px(3.))
                        .right(px(3.))
                        .size(px(6.))
                        .rounded_full()
                        .bg(color),
                ),
            })
            .on_click(cx.listener(move |this, _, _, cx| {
                this.active = ix;
                cx.notify();
            }))
            .when_some(ix, |d, i| {
                d.child(
                    div()
                        .id(("rail-close", i))
                        .absolute()
                        .top(px(-5.))
                        .right(px(-5.))
                        .size(px(16.))
                        .rounded_full()
                        .flex()
                        .items_center()
                        .justify_center()
                        .bg(theme.background)
                        .border_1()
                        .border_color(theme.border)
                        .invisible()
                        .group_hover("rail-item", |s| s.visible())
                        .child(Icon::new(IconName::X).xsmall())
                        .on_click(cx.listener(move |this, _, window, cx| {
                            cx.stop_propagation();
                            this.close_session(i, window, cx);
                        })),
                )
            })
    }
}

impl Render for AppView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let content = match self.active.and_then(|i| self.sessions.get(i)) {
            Some(s) => s.clone().into_any_element(),
            None => self.home.clone().into_any_element(),
        };
        let rail = v_flex()
            .w(px(58.))
            .h_full()
            .items_center()
            .gap_2()
            .py_2()
            .bg(theme.sidebar)
            .border_r_1()
            .border_color(theme.border)
            .child(self.rail_button(None, cx))
            .child(div().w(px(24.)).h(px(1.)).bg(theme.border))
            .children(
                (0..self.sessions.len()).map(|i| self.rail_button(Some(i), cx).into_any_element()),
            )
            .child(
                Button::new("rail-new")
                    .icon(IconName::Plus)
                    .ghost()
                    .small()
                    .tooltip("New connection (⌘N)")
                    .on_click(cx.listener(|this, _, window, cx| this.new_connection(window, cx))),
            );
        let dark = theme.mode.is_dark();
        v_flex()
            .size_full()
            .bg(theme.background)
            .text_color(theme.foreground)
            .key_context("App")
            .track_focus(&self.focus)
            .child(
                TitleBar::new().child(
                    h_flex()
                        .w_full()
                        .pr_2()
                        .gap_2()
                        .child(
                            div()
                                .text_sm()
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .child("Tabula"),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(format!("{} open", self.sessions.len())),
                        )
                        .child(div().flex_1())
                        .child(
                            Button::new("theme")
                                .icon(if dark { IconName::Sun } else { IconName::Moon })
                                .ghost()
                                .small()
                                .tooltip("Toggle theme (⌘⇧L)")
                                .on_click(cx.listener(|_, _, window, cx| toggle_theme(window, cx))),
                        )
                        .child(
                            Button::new("settings")
                                .icon(IconName::Settings)
                                .ghost()
                                .small()
                                .tooltip("Settings (⌘,)")
                                .on_click(|_, window, cx| SettingsView::open(window, cx)),
                        ),
                ),
            )
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .child(rail)
                    .child(div().flex_1().min_w_0().h_full().child(content)),
            )
    }
}

impl AppView {
    fn new_connection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.home.update(cx, |h, cx| h.open_form(None, window, cx));
    }
}

fn toggle_theme(window: &mut Window, cx: &mut App) {
    let mode = if cx.theme().mode.is_dark() {
        ThemeMode::Light
    } else {
        ThemeMode::Dark
    };
    Theme::change(mode, Some(window), cx);
    // A theme switch can reload sizes from the theme file; keep the user's.
    settings::apply_theme(cx);
}

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
    cx.on_action(|_: &NewConnection, cx| with_app(cx, |a, w, cx| a.new_connection(w, cx)));
    cx.on_action(|_: &ToggleTheme, cx| with_app(cx, |_, w, cx| toggle_theme(w, cx)));
    cx.on_action(|_: &OpenSettings, cx| with_app(cx, |_, w, cx| SettingsView::open(w, cx)));

    cx.on_action(|_: &NewQuery, cx| with_session(cx, |s, w, cx| s.new_query(w, cx)));
    cx.on_action(|_: &CloseTab, cx| with_session(cx, |s, w, cx| s.close_active_tab(w, cx)));
    cx.on_action(|_: &NextTab, cx| with_session(cx, |s, w, cx| s.cycle_tab(1, w, cx)));
    cx.on_action(|_: &PrevTab, cx| with_session(cx, |s, w, cx| s.cycle_tab(-1, w, cx)));
    cx.on_action(|_: &Refresh, cx| with_session(cx, |s, w, cx| s.refresh(w, cx)));
    cx.on_action(|_: &SwitchDatabase, cx| with_session(cx, |s, w, cx| s.open_db_picker(w, cx)));
    cx.on_action(|_: &RunQuery, cx| {
        with_session(cx, |s, w, cx| {
            if let Some(q) = s.active_query() {
                q.update(cx, |q, cx| q.run(&RunQuery, w, cx));
            }
        })
    });

    cx.on_action(|_: &Commit, cx| with_table(cx, |t, w, cx| t.commit(&Commit, w, cx)));
    cx.on_action(|_: &Discard, cx| with_table(cx, |t, w, cx| t.discard(&Discard, w, cx)));
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
mod tests {
    use std::time::{Duration, Instant};

    use gpui::{TestAppContext, VisualTestContext};
    use gpui_component::Root;

    use super::*;
    use crate::db::{ConnectionConfig, Driver};

    /// Opens the real app view in a test window, with app-wide shortcuts.
    fn open(cx: &mut TestAppContext) -> (Entity<AppView>, VisualTestContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            cx.bind_keys(crate::ui::key_bindings());
            register_actions(cx);
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
    /// Needs the throwaway Postgres container (TABULA_IT=1, port 55432) with
    /// an extra database: `createdb -U postgres tabula_it_other`.
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

        // Clicking a database switches too.
        cx.simulate_keystrokes("cmd-k");
        cx.run_until_parked();
        let row = cx
            .debug_bounds("db-row-postgres")
            .expect("postgres row is shown");
        cx.simulate_click(row.center(), gpui::Modifiers::none());
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
}
