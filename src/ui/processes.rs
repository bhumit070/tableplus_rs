//! Process list (⌘⇧P, design.md screen 16): the server's sessions, live
//! every 2 s (with Pause), filtered to Active / Idle in transaction / All,
//! and a detail strip for the selected one — its full query, client, start
//! time and the session blocking it. Cancel query and Kill session show the
//! exact statement first, like any other write.

use std::rc::Rc;
use std::time::Duration;

use gpui::{
    App, AppContext as _, ClipboardItem, Context, Entity, FontWeight, Hsla, IntoElement,
    ParentElement, Render, SharedString, Styled, Window, div, prelude::*,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::notification::Notification;
use gpui_component::switch::Switch;
use gpui_component::table::{TableEvent, TableState};
use gpui_component::{ActiveTheme as _, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit_assets::IconName;

use super::Shared;
use super::confirm::ConfirmWrite;
use super::dialog;
use super::grid::{Grid, data_table};
use super::theme::{dp, tokens};
use crate::db::safety::{Gate, gate_write};
use crate::db::{ColumnInfo, Connection, Driver, QueryResult};
use crate::runtime;

/// How often the list refreshes while Live is on.
const LIVE_EVERY: Duration = Duration::from_secs(2);

#[derive(Clone, Copy, PartialEq, Eq)]
enum Show {
    Active,
    IdleInTransaction,
    All,
}

pub struct Processes {
    shared: Rc<Shared>,
    grid: Entity<TableState<Grid>>,
    /// The server's last answer, before the Active / Idle filter.
    all: QueryResult,
    show: Show,
    error: Option<String>,
    loading: bool,
    live: bool,
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
        let view = cx.new(|cx| {
            let mut colours = Grid::default();
            colours.tint = Some(Rc::new(tint));
            let grid = cx.new(|cx| {
                TableState::new(colours, window, cx)
                    .row_selectable(true)
                    .col_resizable(true)
            });
            // The detail strip follows the selection.
            cx.subscribe(&grid, |_, _, event: &TableEvent, cx| {
                if matches!(event, TableEvent::SelectRow(_) | TableEvent::SelectCell(..)) {
                    cx.notify();
                }
            })
            .detach();
            let mut list = Processes {
                shared,
                grid,
                all: QueryResult::default(),
                show: Show::All,
                error: None,
                loading: false,
                live: true,
            };
            list.refresh(cx);
            // Refresh while Live, until the dialog (and this view) is gone.
            cx.spawn(async move |this, cx| {
                loop {
                    cx.background_executor().timer(LIVE_EVERY).await;
                    let alive = this
                        .update(cx, |list, cx| {
                            if list.live && !list.loading {
                                list.refresh(cx);
                            }
                        })
                        .is_ok();
                    if !alive {
                        break;
                    }
                }
            })
            .detach();
            list
        });
        let width = dialog::width(1040., cx);
        window.open_dialog(cx, move |d, _, cx| {
            dialog::shell(d, width, cx).child(view.clone())
        });
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        self.loading = true;
        // Placeholder rows the first time; later refreshes keep the list.
        self.grid.update(cx, |table, cx| {
            if table.delegate().rows.is_empty() {
                table.delegate_mut().set_loading(true);
                cx.notify();
            }
        });
        let conn = self.shared.conn.clone();
        let task = runtime::spawn(async move { conn.read().await.processes().await });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |list, cx| {
                list.loading = false;
                list.grid
                    .update(cx, |table, _| table.delegate_mut().set_loading(false));
                match result {
                    Ok(sessions) => {
                        list.error = None;
                        list.all = sessions;
                        list.show_filtered(cx);
                    }
                    Err(e) => list.error = Some(format!("{e:#}")),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn column(&self, name: &str) -> Option<usize> {
        self.all.columns.iter().position(|c| c == name)
    }

    /// Whether `row` passes the Active / Idle in transaction filter.
    fn passes(&self, row: &[Option<String>]) -> bool {
        let state = self
            .column("state")
            .and_then(|i| row.get(i).cloned().flatten())
            .unwrap_or_default();
        let command = self
            .column("command")
            .and_then(|i| row.get(i).cloned().flatten())
            .unwrap_or_default();
        match (self.show, self.shared.cfg.driver) {
            (Show::All, _) => true,
            (Show::Active, Driver::Postgres) => state == "active",
            (Show::Active, Driver::MySql) => command != "Sleep",
            (Show::IdleInTransaction, Driver::Postgres) => state.starts_with("idle in transaction"),
            _ => true,
        }
    }

    /// Puts the filtered sessions in the grid, keeping the selection on the
    /// same session.
    fn show_filtered(&mut self, cx: &mut Context<Self>) {
        let selected = self.selected_id(cx);
        let shown = QueryResult {
            rows: self
                .all
                .rows
                .iter()
                .filter(|row| self.passes(row))
                .cloned()
                .collect(),
            ..self.all.clone()
        };
        self.grid.update(cx, |table, cx| {
            table.delegate_mut().set_result(&shown);
            // States read as pills (active, idle in transaction, Sleep…).
            let state = shown.columns.iter().position(|column| column == "state");
            if let Some(state) = state {
                let mut states: Vec<String> = shown
                    .rows
                    .iter()
                    .filter_map(|row| row[state].clone())
                    .collect();
                states.sort();
                states.dedup();
                table.delegate_mut().set_choices(&[ColumnInfo {
                    name: "state".into(),
                    data_type: String::new(),
                    nullable: true,
                    default: None,
                    is_pk: false,
                    enum_values: states,
                    comment: None,
                }]);
            }
            let again = selected.and_then(|id| {
                shown
                    .rows
                    .iter()
                    .position(|row| row[0].as_deref() == Some(&id))
            });
            table.refresh(cx);
            if let Some(row) = again {
                table.set_selected_row(row, cx);
            }
        });
    }

    fn selected_id(&self, cx: &App) -> Option<String> {
        let table = self.grid.read(cx);
        let row = table.selected_row()?;
        table.delegate().cell_string(row, 0)
    }

    /// A value of the selected session, by column name.
    fn selected_value(&self, name: &str, cx: &App) -> Option<String> {
        let table = self.grid.read(cx);
        let row = table.selected_row()?;
        let grid = table.delegate();
        let column = grid.columns.iter().position(|c| c == name)?;
        grid.cell_string(row, column).filter(|v| !v.is_empty())
    }

    /// "Blocked by PID n": select that session (switching to All if the
    /// filter hides it).
    fn select_session(&mut self, id: &str, cx: &mut Context<Self>) {
        let find = |list: &Self, cx: &App| {
            let grid = list.grid.read(cx).delegate();
            (0..grid.rows.len()).find(|row| grid.cell_string(*row, 0).as_deref() == Some(id))
        };
        if find(self, cx).is_none() {
            self.show = Show::All;
            self.show_filtered(cx);
        }
        if let Some(row) = find(self, cx) {
            self.grid
                .update(cx, |table, cx| table.set_selected_row(row, cx));
        }
        cx.notify();
    }

    fn kill(&mut self, whole: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.selected_id(cx) else {
            window.push_notification("Select a process first", cx);
            return;
        };
        let cfg = self.shared.cfg.clone();
        if let Gate::Block(why) = gate_write(&cfg) {
            window.push_notification(Notification::warning(why), cx);
            return;
        }
        let statement = Connection::kill_statement(cfg.driver, &id, whole);
        let me = cx.entity().downgrade();
        let conn = self.shared.conn.clone();
        let what = if whole {
            "Kill session"
        } else {
            "Cancel query"
        };
        // Always confirmed, with the statement, whatever the safe mode:
        // another session's work is at stake.
        ConfirmWrite::open(
            &cfg,
            what,
            statement,
            window,
            cx,
            Rc::new(move |window, cx| {
                let (conn, id) = (conn.clone(), id.clone());
                let task = runtime::spawn(async move { conn.read().await.kill(&id, whole).await });
                let me = me.clone();
                window
                    .spawn(cx, async move |cx| {
                        let result = task.await;
                        cx.update(|window, cx| {
                            if let Err(e) = result {
                                window.push_notification(Notification::error(format!("{e:#}")), cx);
                            }
                            me.update(cx, |list, cx| list.refresh(cx)).ok();
                        })
                        .ok();
                    })
                    .detach();
            }),
        );
    }

    fn render_detail(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let t = tokens(cx);
        let mono = cx.theme().mono_font_family.clone();
        let Some(id) = self.selected_id(cx) else {
            return div()
                .h(dp(96.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(dp(8.))
                .border_1()
                .border_color(t.border)
                .text_size(dp(12.))
                .text_color(t.text_faint)
                .child("Select a session to see its query")
                .into_any_element();
        };
        let query = self
            .selected_value("query", cx)
            .or_else(|| self.selected_value("info", cx))
            .unwrap_or_else(|| "(no query)".into());
        let facts = ["user", "client", "started", "running_for", "waiting_on"]
            .iter()
            .filter_map(|name| self.selected_value(name, cx).map(|v| (*name, v)))
            .map(|(name, value)| format!("{} {value}", name.replace('_', " ")))
            .collect::<Vec<_>>()
            .join("  ·  ");
        let blockers: Vec<String> = self
            .selected_value("blocked_by", cx)
            .map(|list| list.split(", ").map(str::to_string).collect())
            .unwrap_or_default();
        let copy = query.clone();
        v_flex()
            .gap(dp(8.))
            .p(dp(12.))
            .rounded(dp(8.))
            .border_1()
            .border_color(t.border)
            .bg(t.canvas)
            .child(
                h_flex()
                    .gap(dp(10.))
                    .text_size(dp(12.))
                    .text_color(t.text_muted)
                    .child(
                        div()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(t.text)
                            .child(format!("Session {id}")),
                    )
                    .child(div().flex_1().min_w_0().truncate().child(facts))
                    .children(blockers.into_iter().map(|blocker| {
                        let target = blocker.clone();
                        Button::new(SharedString::from(format!("blocked-by-{blocker}")))
                            .cursor_pointer()
                            .icon(IconName::Lock)
                            .label(format!("Blocked by PID {blocker}"))
                            .outline()
                            .xsmall()
                            .on_click(
                                cx.listener(move |list, _, _, cx| list.select_session(&target, cx)),
                            )
                    }))
                    .child(
                        Button::new("copy-session-query")
                            .cursor_pointer()
                            .icon(IconName::Copy)
                            .ghost()
                            .xsmall()
                            .tooltip("Copy the query")
                            .on_click(move |_, window, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(copy.clone()));
                                window.push_notification("Copied", cx);
                            }),
                    ),
            )
            .child(
                div()
                    .id("session-query")
                    .max_h(dp(72.))
                    .overflow_y_scroll()
                    .font_family(mono)
                    .text_size(dp(11.5))
                    .text_color(t.text_secondary)
                    .child(query),
            )
            .into_any_element()
    }
}

impl Render for Processes {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = tokens(cx);
        let driver = self.shared.cfg.driver;
        let shown = self.grid.read(cx).delegate().rows.len();
        let total = self.all.rows.len();
        let me = cx.entity();
        // Postgres knows "idle in transaction"; MySQL only busy or asleep.
        let filters: &[(Show, &str)] = match driver {
            Driver::Postgres => &[
                (Show::Active, "Active"),
                (Show::IdleInTransaction, "Idle in tx"),
                (Show::All, "All"),
            ],
            Driver::MySql => &[(Show::Active, "Active"), (Show::All, "All")],
            _ => &[],
        };
        let filter_index = filters
            .iter()
            .position(|(show, _)| *show == self.show)
            .unwrap_or(0);
        let labels: Vec<&str> = filters.iter().map(|(_, label)| *label).collect();
        let options: Vec<Show> = filters.iter().map(|(show, _)| *show).collect();
        v_flex()
            .gap(dp(12.))
            .h(dp(600.))
            .child(dialog::title(
                format!("Processes · {}", self.shared.cfg.display_name()),
                Some(
                    if shown == total {
                        format!("{total} other sessions")
                    } else {
                        format!("{shown} of {total} other sessions")
                    }
                    .into(),
                ),
                cx,
            ))
            .child(
                h_flex()
                    .gap(dp(10.))
                    .when(!labels.is_empty(), |row| {
                        row.child(
                            dialog::segmented(
                                "process-filter",
                                &labels,
                                filter_index,
                                Rc::new(move |index, _, cx| {
                                    let show = options[index];
                                    me.update(cx, |list, cx| {
                                        list.show = show;
                                        list.show_filtered(cx);
                                        cx.notify();
                                    })
                                }),
                                cx,
                            )
                            .w(dp(70. * labels.len() as f32 + 20.)),
                        )
                    })
                    .child(
                        Switch::new("live-processes")
                            .checked(self.live)
                            .label(if self.live {
                                "Live · every 2 s"
                            } else {
                                "Paused"
                            })
                            .on_click(cx.listener(|list, on: &bool, _, cx| {
                                list.live = *on;
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("refresh-processes")
                            .cursor_pointer()
                            .icon(IconName::RefreshCw)
                            .ghost()
                            .small()
                            .loading(self.loading)
                            .tooltip("Refresh now")
                            .on_click(cx.listener(|list, _, _, cx| list.refresh(cx))),
                    )
                    .child(div().flex_1())
                    .when(driver != Driver::Redis, |row| {
                        row.child(
                            Button::new("cancel-process")
                                .cursor_pointer()
                                .label("Cancel query")
                                .outline()
                                .small()
                                .on_click(
                                    cx.listener(|list, _, window, cx| list.kill(false, window, cx)),
                                ),
                        )
                    })
                    .child(
                        Button::new("kill-process")
                            .cursor_pointer()
                            .label("Kill session")
                            .danger()
                            .small()
                            .on_click(
                                cx.listener(|list, _, window, cx| list.kill(true, window, cx)),
                            ),
                    ),
            )
            .when_some(self.error.clone(), |column, e| {
                column.child(div().text_size(dp(12.)).text_color(t.danger).child(e))
            })
            .child(div().flex_1().min_h_0().child(data_table(&self.grid, cx)))
            .child(self.render_detail(cx))
            .child(
                div()
                    .text_size(dp(11.))
                    .text_color(t.text_faint)
                    .child("Your own session isn't listed, so it can't be killed by mistake."),
            )
    }
}

/// Seconds in Postgres's `HH:MM:SS` or MySQL's plain number.
fn running_seconds(text: &str) -> Option<u64> {
    let parts: Vec<u64> = text
        .split(':')
        .map(|part| part.trim().parse::<u64>().ok())
        .collect::<Option<_>>()?;
    Some(parts.iter().fold(0, |total, part| total * 60 + part))
}

/// Statements running over a minute in red, sessions waiting on a lock in
/// amber (design.md screen 16).
fn tint(column: &str, value: &str, cx: &App) -> Option<Hsla> {
    let t = tokens(cx);
    match column {
        "running_for" | "seconds" => running_seconds(value)
            .filter(|seconds| *seconds > 60)
            .map(|_| t.danger),
        "waiting_on" if value.starts_with("Lock") => Some(t.warning),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_running_times() {
        assert_eq!(running_seconds("00:01:05"), Some(65));
        assert_eq!(running_seconds("42"), Some(42));
        assert_eq!(running_seconds(""), None);
    }
}
