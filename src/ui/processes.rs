//! Process list (⌘⇧P): the server's sessions, refreshed every few seconds,
//! with Cancel Query / Kill Connection for the selected one.

use std::rc::Rc;
use std::time::Duration;

use gpui::{
    App, AppContext as _, Context, Entity, IntoElement, ParentElement, Render, SharedString,
    Styled, Window, div, prelude::*, px,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::checkbox::Checkbox;
use gpui_component::notification::Notification;
use gpui_component::table::TableState;
use gpui_component::{ActiveTheme as _, Sizable as _, WindowExt as _, h_flex, v_flex};

use super::Shared;
use super::grid::{Grid, data_table};
use crate::db::{Driver, EnvTag};
use crate::runtime;

pub struct Processes {
    shared: Rc<Shared>,
    grid: Entity<TableState<Grid>>,
    error: Option<String>,
    loading: bool,
    auto: bool,
}

impl Processes {
    pub fn open(shared: Rc<Shared>, window: &mut Window, cx: &mut App) {
        if shared.cfg.driver == Driver::Sqlite {
            window.push_notification("SQLite has no server processes", cx);
            return;
        }
        if shared.cfg.driver == Driver::Mongo {
            window.push_notification("The process list isn't available for MongoDB yet", cx);
            return;
        }
        let name = shared.cfg.display_name();
        let view = cx.new(|cx| {
            let grid = cx.new(|cx| {
                TableState::new(Grid::default(), window, cx)
                    .row_selectable(true)
                    .col_resizable(true)
            });
            let mut p = Processes {
                shared,
                grid,
                error: None,
                loading: false,
                auto: true,
            };
            p.refresh(cx);
            // Auto-refresh until the dialog (and this view) is gone.
            cx.spawn(async move |this, cx| {
                loop {
                    cx.background_executor().timer(Duration::from_secs(3)).await;
                    let alive = this
                        .update(cx, |p, cx| {
                            if p.auto && !p.loading {
                                p.refresh(cx);
                            }
                        })
                        .is_ok();
                    if !alive {
                        break;
                    }
                }
            })
            .detach();
            p
        });
        window.open_dialog(cx, move |dialog, _, _| {
            dialog
                .title(format!("Processes · {name}"))
                .w(px(980.))
                .child(view.clone())
        });
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        self.loading = true;
        let conn = self.shared.conn.clone();
        let task = runtime::spawn(async move { conn.read().await.processes().await });
        cx.spawn(async move |this, cx| {
            let res = task.await;
            this.update(cx, |p, cx| {
                p.loading = false;
                match res {
                    Ok(r) => {
                        p.error = None;
                        // Keep the selection on the same process across refreshes.
                        let selected = p.selected_id(cx);
                        p.grid.update(cx, |t, cx| {
                            t.delegate_mut().set_result(&r);
                            let again = selected.and_then(|id| {
                                r.rows.iter().position(|row| row[0].as_deref() == Some(&id))
                            });
                            t.refresh(cx);
                            if let Some(i) = again {
                                t.set_selected_row(i, cx);
                            }
                        });
                    }
                    Err(e) => p.error = Some(format!("{e:#}")),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn selected_id(&self, cx: &App) -> Option<String> {
        let t = self.grid.read(cx);
        let r = t.selected_row()?;
        t.delegate().cell_string(r, 0)
    }

    fn kill(&mut self, whole: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.selected_id(cx) else {
            window.push_notification("Select a process first", cx);
            return;
        };
        let what = if whole {
            "Kill connection"
        } else {
            "Cancel the running query of"
        };
        let prod = if self.shared.cfg.tag == EnvTag::Production {
            " on PRODUCTION"
        } else {
            ""
        };
        let me = cx.entity().downgrade();
        let conn = self.shared.conn.clone();
        let text: SharedString = format!("{what} process {id}{prod}?").into();
        window.open_alert_dialog(cx, move |d, _, _| {
            let (me, conn, id) = (me.clone(), conn.clone(), id.clone());
            d.title(if whole {
                "Kill Connection?"
            } else {
                "Cancel Query?"
            })
            .description(text.clone())
            .show_cancel(true)
            .on_ok(move |_, window, cx| {
                let (conn, id) = (conn.clone(), id.clone());
                let task = runtime::spawn(async move { conn.read().await.kill(&id, whole).await });
                let me = me.clone();
                window
                    .spawn(cx, async move |cx| {
                        let res = task.await;
                        cx.update(|window, cx| {
                            if let Err(e) = res {
                                window.push_notification(Notification::error(format!("{e:#}")), cx);
                            }
                            me.update(cx, |p, cx| p.refresh(cx)).ok();
                        })
                        .ok();
                    })
                    .detach();
                true
            })
        });
    }
}

impl Render for Processes {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let redis = self.shared.cfg.driver == Driver::Redis;
        let count = self.grid.read(cx).delegate().rows.len();
        v_flex()
            .gap_2()
            .h(px(480.))
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("refresh-processes")
                            .cursor_pointer()
                            .label("Refresh")
                            .small()
                            .loading(self.loading)
                            .on_click(cx.listener(|p, _, _, cx| p.refresh(cx))),
                    )
                    .child(
                        Checkbox::new("auto-refresh")
                            .cursor_pointer()
                            .checked(self.auto)
                            .label("Auto-refresh")
                            .on_click(cx.listener(|p, checked: &bool, _, cx| {
                                p.auto = *checked;
                                cx.notify();
                            })),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(format!("{count} other session(s)")),
                    )
                    .child(div().flex_1())
                    .when(!redis, |d| {
                        d.child(
                            Button::new("cancel-process")
                                .cursor_pointer()
                                .label("Cancel Query")
                                .small()
                                .on_click(
                                    cx.listener(|p, _, window, cx| p.kill(false, window, cx)),
                                ),
                        )
                    })
                    .child(
                        Button::new("kill-process")
                            .cursor_pointer()
                            .label("Kill Connection")
                            .danger()
                            .small()
                            .on_click(cx.listener(|p, _, window, cx| p.kill(true, window, cx))),
                    ),
            )
            .when_some(self.error.clone(), |d, e| {
                d.child(div().text_sm().text_color(theme.danger).child(e))
            })
            .child(div().flex_1().min_h_0().child(data_table(&self.grid, cx)))
    }
}
