//! SQL / command editor with a results pane. ⌘↵ runs the selection, or the
//! whole editor when nothing is selected. Each statement gets a result tab.

use std::rc::Rc;
use std::sync::Arc;

use gpui::{
    App, ClipboardItem, Context, Entity, FocusHandle, Focusable, IntoElement, ParentElement,
    Render, SharedString, Styled, Subscription, Window, div, prelude::*, px, rems,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::dialog::DialogFooter;
use gpui_component::input::{Editor, EditorState, Input, InputState};
use gpui_component::menu::PopupMenuItem;
use gpui_component::notification::Notification;
use gpui_component::tab::{Tab, TabBar};
use gpui_component::table::{TableEvent, TableState};
use gpui_component::{
    ActiveTheme as _, Selectable as _, Sizable as _, WindowExt as _, h_flex, resizable_panel,
    v_flex, v_resizable,
};
use gpui_kit_assets::IconName;

use super::cell_edit::{self, EditStart};
use super::copy;
use super::row_panel;
use super::grid::{Grid, data_table};
use super::{RunQuery, Shared};
use crate::db::{Cancel, ColumnInfo, DbObject, Driver, ObjectKind, QueryResult};
use crate::{history, runtime};

pub struct QueryTab {
    shared: Rc<Shared>,
    pub title: String,
    editor: Entity<EditorState>,
    results: Vec<QueryResult>,
    active: usize,
    grid: Entity<TableState<Grid>>,
    running: bool,
    /// Stops the running batch; `Some` while running.
    cancel: Option<Arc<Cancel>>,
    error: Option<String>,
    elapsed: u128,
    focus: FocusHandle,
    /// The text last run, re-run after committing edits.
    last_run: String,
    /// Set when the shown result comes from one table and can be edited.
    target: Option<EditTarget>,
    /// Why the shown result is read-only, if it is.
    edit_note: Option<String>,
    /// Bumped per shown result; drops stale table lookups.
    edit_gen: u64,
    edit_sub: Option<Subscription>,
    /// Row detail panel beside the results.
    pub show_detail: bool,
    /// A commit is on its way; ⌘S again mustn't send it twice.
    committing: bool,
    /// The last EXPLAIN, shown instead of results until the next run.
    plan: Option<crate::db::explain::PlanNode>,
    /// Set once this tab's text is a saved query; ⌘⇧S then updates it.
    saved: Option<crate::saved::SavedQuery>,
    _subs: Vec<Subscription>,
}

#[derive(Clone)]
struct EditTarget {
    obj: DbObject,
    columns: Vec<ColumnInfo>,
}

/// Which result columns are real table columns (editable), and which are
/// primary-key columns; or why the result can't be edited.
fn map_columns(
    result_cols: &[String],
    table: &str,
    table_cols: &[ColumnInfo],
) -> Result<(Vec<bool>, Vec<bool>), String> {
    if table_cols.is_empty() {
        return Err(format!("Read-only: couldn't find table {table}"));
    }
    let pks: Vec<&str> = table_cols
        .iter()
        .filter(|c| c.is_pk)
        .map(|c| c.name.as_str())
        .collect();
    if pks.is_empty() {
        return Err(format!("Read-only: {table} has no primary key"));
    }
    if let Some(missing) = pks.iter().find(|pk| !result_cols.iter().any(|c| c == *pk)) {
        return Err(format!(
            "Read-only: select the primary key ({missing}) to edit"
        ));
    }
    let mut seen = std::collections::HashSet::new();
    let editable = result_cols
        .iter()
        .map(|c| table_cols.iter().any(|t| &t.name == c) && seen.insert(c.clone()))
        .collect();
    let pk = result_cols
        .iter()
        .map(|c| pks.contains(&c.as_str()))
        .collect();
    Ok((editable, pk))
}

fn placeholder(d: Driver) -> &'static str {
    match d {
        Driver::Redis => "GET mykey\nHGETALL user:1\nSCAN 0 MATCH user:* COUNT 100",
        Driver::Mongo => "db.users.find({ age: { $gt: 30 } }).sort({ name: 1 }).limit(50)",
        _ => "SELECT * FROM users WHERE id = 1;",
    }
}

impl QueryTab {
    pub fn new(
        shared: Rc<Shared>,
        n: usize,
        initial: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let lang = shared.cfg.driver.editor_language();
        let editor = cx.new(|cx| {
            EditorState::new(window, cx)
                .language(lang)
                .line_number(true)
                .soft_wrap(false)
                .placeholder(placeholder(shared.cfg.driver))
                .default_value(initial.to_string())
        });
        let provider = super::completion::Provider {
            schema: shared.schema.clone(),
            driver: shared.cfg.driver,
        };
        editor.update(cx, |e, _| {
            e.lsp_mut().completion_provider = Some(Rc::new(provider))
        });
        let weak = cx.entity().downgrade();
        let mut grid = Grid::default();
        grid.on_menu = Some(Rc::new(move |row, menu, _, _| {
            let w = weak.clone();
            menu.item(
                PopupMenuItem::new("Copy Cell").on_click(move |_, window, cx| {
                    w.update(cx, |t, cx| {
                        let g = t.grid.read(cx);
                        let col = g.selected_cell().map(|(_, c)| c).unwrap_or(0);
                        let v = g.delegate().cell_string(row, col).unwrap_or_default();
                        cx.write_to_clipboard(ClipboardItem::new_string(v));
                        window.push_notification("Copied", cx);
                    })
                    .ok();
                }),
            )
            .separator()
            .map(|menu| copy::copy_items(menu, weak.clone(), row))
        }));
        let grid = cx.new(|cx| {
            TableState::new(grid, window, cx)
                .cell_selectable(true)
                .row_selectable(true)
                .col_resizable(true)
                .sortable(true)
        });
        let subs = vec![
            cx.subscribe_in(&grid, window, |this, _, ev: &TableEvent, window, cx| {
                match ev {
                    TableEvent::DoubleClickedCell(r, c) => {
                        cell_edit::start_edit(this, *r, *c, EditStart::Keep, window, cx)
                    }
                    // The row detail panel follows the selection.
                    TableEvent::SelectRow(_) | TableEvent::SelectCell(..) => cx.notify(),
                    _ => {}
                }
            }),
        ];
        Self {
            shared,
            title: format!("Query {n}"),
            editor,
            results: Vec::new(),
            active: 0,
            grid,
            running: false,
            cancel: None,
            show_detail: false,
            saved: None,
            plan: None,
            committing: false,
            error: None,
            elapsed: 0,
            focus: cx.focus_handle(),
            last_run: String::new(),
            target: None,
            edit_note: None,
            edit_gen: 0,
            edit_sub: None,
            _subs: subs,
        }
    }

    pub fn focus_editor(this: &Entity<Self>, window: &mut Window, cx: &mut App) {
        let editor = this.read(cx).editor.clone();
        editor.update(cx, |e, cx| e.focus(window, cx));
    }

    pub fn run(&mut self, _: &RunQuery, window: &mut Window, cx: &mut Context<Self>) {
        if self.running {
            return;
        }
        let e = self.editor.read(cx);
        let sel = e.selected_value();
        let text = if sel.trim().is_empty() {
            e.value().to_string()
        } else {
            sel.to_string()
        };
        if text.trim().is_empty() {
            return;
        }
        if self.blocked_by_edits(window, cx) {
            return;
        }
        let gate = crate::db::safety::gate_query(&self.shared.cfg, &text);
        let weak = cx.entity().downgrade();
        let t2 = text.clone();
        if super::guarded(gate, &self.shared.cfg, "Run", text.clone(), window, cx, move |w, cx| {
            weak.update(cx, |t, cx| t.execute(t2.clone(), w, cx)).ok();
        }) {
            self.execute(text, window, cx);
        }
    }

    /// ⌘E: the plan of the selected / first statement. `analyze` runs it
    /// (inside a transaction that's rolled back, so writes don't stick).
    pub fn explain(&mut self, analyze: bool, window: &mut Window, cx: &mut Context<Self>) {
        use crate::db::explain;
        if self.running {
            return;
        }
        let driver = self.shared.cfg.driver;
        if !matches!(driver, Driver::Postgres | Driver::MySql | Driver::Sqlite) {
            window.push_notification("EXPLAIN is available for Postgres, MySQL and SQLite", cx);
            return;
        }
        let e = self.editor.read(cx);
        let sel = e.selected_value().to_string();
        let text = if sel.trim().is_empty() { e.value().to_string() } else { sel };
        let stmts = crate::db::split::split_statements(&text, driver == Driver::MySql);
        let Some(stmt) = stmts.into_iter().find(|s| !s.trim().is_empty()) else {
            return;
        };
        let wrapped = match explain::explain_script(driver, &stmt, analyze) {
            Ok(w) => w,
            Err(e) => {
                window.push_notification(Notification::error(format!("{e:#}")), cx);
                return;
            }
        };
        // ANALYZE executes the statement: the safe mode decides as for a run.
        if analyze {
            let gate = crate::db::safety::gate_query(&self.shared.cfg, &stmt);
            let weak = cx.entity().downgrade();
            let w2 = wrapped.clone();
            if !super::guarded(gate, &self.shared.cfg, "Run EXPLAIN ANALYZE", stmt.clone(), window, cx, move |w, cx| {
                weak.update(cx, |t, cx| t.run_explain(w2.clone(), w, cx)).ok();
            }) {
                return;
            }
        }
        self.run_explain(wrapped, window, cx);
    }

    fn run_explain(&mut self, sql: String, window: &mut Window, cx: &mut Context<Self>) {
        let driver = self.shared.cfg.driver;
        self.running = true;
        self.error = None;
        cx.notify();
        let conn = self.shared.conn.clone();
        let cancel = Arc::new(Cancel::default());
        self.cancel = Some(cancel.clone());
        let task = runtime::spawn(async move {
            let rs = conn.read().await.execute_cancellable(&sql, &cancel).await?;
            let r = rs
                .iter()
                .find(|r| r.statement.trim_start().to_ascii_uppercase().starts_with("EXPLAIN"))
                .cloned()
                .unwrap_or_default();
            crate::db::explain::parse(driver, &r)
        });
        cx.spawn_in(window, async move |this, cx| {
            let res = task.await;
            this.update(cx, |this, cx| {
                this.running = false;
                this.cancel = None;
                match res {
                    Ok(p) => this.plan = Some(p),
                    Err(e) => this.error = Some(format!("{e:#}")),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn render_plan(&self, plan: &crate::db::explain::PlanNode, cx: &mut Context<Self>) -> gpui::AnyElement {
        let theme = cx.theme().clone();
        let nodes = plan.flatten();
        let max = nodes
            .iter()
            .map(|(_, n)| n.self_weight())
            .fold(0.0_f64, f64::max)
            .max(f64::EPSILON);
        let fmt = |v: f64| {
            if v >= 100.0 { format!("{v:.0}") } else { format!("{v:.2}") }
        };
        div()
            .id("plan")
            .size_full()
            .overflow_y_scroll()
            .p_2()
            .child(v_flex().gap_1().children(nodes.into_iter().map(|(depth, n)| {
                let share = n.self_weight() / max;
                let bar = if share > 0.66 {
                    theme.danger
                } else if share > 0.33 {
                    theme.warning
                } else {
                    theme.success
                };
                let mut stats = vec![];
                if let Some(c) = n.cost {
                    stats.push(format!("cost {}", fmt(c)));
                }
                if let Some(r) = n.rows {
                    stats.push(format!("est. rows {}", fmt(r)));
                }
                if let Some(t) = n.actual_ms {
                    stats.push(format!("{} ms", fmt(t)));
                }
                if let Some(r) = n.actual_rows {
                    stats.push(format!("rows {}", fmt(r)));
                }
                h_flex()
                    .pl(px(16. * depth as f32))
                    .gap_2()
                    .items_start()
                    .child(
                        div()
                            .mt(px(6.))
                            .w(px(48.))
                            .h(px(6.))
                            .flex_none()
                            .rounded_sm()
                            .bg(theme.border)
                            .child(div().h_full().rounded_sm().w(px(48. * share as f32)).bg(bar)),
                    )
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(
                                h_flex()
                                    .gap_2()
                                    .child(
                                        div()
                                            .text_sm()
                                            .font_weight(gpui::FontWeight::SEMIBOLD)
                                            .child(n.label.clone()),
                                    )
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(theme.muted_foreground)
                                            .child(stats.join(" · ")),
                                    ),
                            )
                            .children(n.details.iter().map(|d| {
                                div()
                                    .text_xs()
                                    .font_family(theme.mono_font_family.clone())
                                    .text_color(theme.muted_foreground)
                                    .child(d.clone())
                            })),
                    )
            })))
            .into_any_element()
    }

    fn execute(&mut self, text: String, window: &mut Window, cx: &mut Context<Self>) {
        self.plan = None;
        self.last_run = text.clone();
        self.running = true;
        self.error = None;
        cx.notify();
        let conn = self.shared.conn.clone();
        let start = std::time::Instant::now();
        let cancel = Arc::new(Cancel::default());
        self.cancel = Some(cancel.clone());
        let conn_id = self.shared.cfg.id.clone();
        let task = runtime::spawn(async move {
            let c = conn.read().await;
            let ran_at = history::now();
            let res = c.execute_cancellable(&text, &cancel).await;
            let entry = history::Entry {
                conn_id,
                database: c.current_database(),
                sql: text.trim().to_string(),
                ran_at,
                elapsed_ms: start.elapsed().as_millis() as i64,
                rows: res.as_ref().ok().and_then(|r| r.last()).map(|r| {
                    if r.columns.is_empty() {
                        r.affected.unwrap_or(0) as i64
                    } else {
                        r.rows.len() as i64
                    }
                }),
                error: res.as_ref().err().map(|e| format!("{e:#}")),
            };
            drop(c);
            // History is a convenience: never fail the run over it.
            if let Err(e) = history::record(entry).await {
                eprintln!("query history: {e:#}");
            }
            res
        });
        cx.spawn_in(window, async move |this, cx| {
            let res = task.await;
            this.update_in(cx, |this, window, cx| {
                this.running = false;
                this.cancel = None;
                this.elapsed = start.elapsed().as_millis();
                match res {
                    Ok(results) => {
                        // Show the last statement that returned rows, else the last one.
                        this.active = results
                            .iter()
                            .rposition(|r| !r.columns.is_empty())
                            .unwrap_or(results.len().saturating_sub(1));
                        this.results = results;
                        this.show_active(cx);
                    }
                    Err(e) => {
                        this.error = Some(format!("{e:#}"));
                        let cancelled = e.root_cause().to_string().starts_with(crate::db::cancel::CANCELLED);
                        if !cancelled {
                            window.push_notification(Notification::error("Query failed"), cx);
                        }
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Opened from a saved query: the tab takes its name.
    pub fn set_saved(&mut self, q: crate::saved::SavedQuery) {
        self.title = q.name.clone();
        self.saved = Some(q);
    }

    /// ⌘⇧S: updates this tab's saved query, or asks for a name.
    pub fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let sql = self.editor.read(cx).value().to_string();
        if sql.trim().is_empty() {
            return;
        }
        if let Some(q) = &self.saved {
            let q = crate::saved::SavedQuery { sql, ..q.clone() };
            match crate::saved::upsert(q.clone()) {
                Ok(()) => {
                    self.saved = Some(q);
                    window.push_notification("Saved", cx);
                }
                Err(e) => window.push_notification(Notification::error(format!("{e:#}")), cx),
            }
            return;
        }
        let name = cx.new(|cx| InputState::new(window, cx).placeholder("Name, e.g. Active users"));
        let me = cx.entity().downgrade();
        let conn_id = self.shared.cfg.id.clone();
        let first = std::cell::Cell::new(true);
        window.open_dialog(cx, move |dialog, window, cx| {
            if first.replace(false) {
                name.update(cx, |n, cx| n.focus(window, cx));
            }
            // `all` = visible on every connection.
            let save = {
                let (name, me, sql, conn_id) =
                    (name.clone(), me.clone(), sql.clone(), conn_id.clone());
                move |all: bool, window: &mut Window, cx: &mut App| -> bool {
                    let n = name.read(cx).value().trim().to_string();
                    if n.is_empty() {
                        return false;
                    }
                    let q = crate::saved::SavedQuery {
                        id: uuid::Uuid::new_v4().to_string(),
                        name: n,
                        sql: sql.clone(),
                        conn_id: (!all).then(|| conn_id.clone()),
                    };
                    if let Err(e) = crate::saved::upsert(q.clone()) {
                        window.push_notification(Notification::error(format!("{e:#}")), cx);
                        return true;
                    }
                    me.update(cx, |t, cx| {
                        t.set_saved(q);
                        cx.notify();
                    })
                    .ok();
                    window.push_notification("Query saved (⌘⇧O to open)", cx);
                    true
                }
            };
            let s1 = Rc::new(save);
            let s_ok = s1.clone();
            let s_all = s1.clone();
            dialog
                .title("Save Query")
                .w(px(420.))
                .child(Input::new(&name))
                .on_ok(move |_, window, cx| s_ok(false, window, cx))
                .footer(
                    DialogFooter::new()
                        .child(
                            Button::new("save-all")
                                .label("Save for All Connections")
                                .small()
                                .on_click(move |_, window, cx| {
                                    if s_all(true, window, cx) {
                                        window.close_dialog(cx);
                                    }
                                }),
                        )
                        .child(
                            Button::new("save-one")
                                .label("Save")
                                .primary()
                                .small()
                                .on_click(move |_, window, cx| {
                                    if s1(false, window, cx) {
                                        window.close_dialog(cx);
                                    }
                                }),
                        ),
                )
        });
    }

    /// ⌘⇧F: formats the selection, or the whole editor (undoable).
    pub fn format(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if matches!(self.shared.cfg.driver, Driver::Redis | Driver::Mongo) {
            return;
        }
        let mysql = self.shared.cfg.driver == Driver::MySql;
        self.editor.update(cx, |e, cx| {
            let sel = e.selected_value().to_string();
            if sel.trim().is_empty() {
                let all = e.value().to_string();
                let f = crate::db::format::format_sql(&all, mysql);
                if f != all {
                    e.replace_all(f, window, cx);
                }
            } else {
                e.replace(crate::db::format::format_sql(&sel, mysql), window, cx);
            }
        });
    }

    /// ⌘. / Stop: cancels the running batch.
    pub fn cancel(&mut self, cx: &mut Context<Self>) {
        let Some(cancel) = self.cancel.clone() else {
            return;
        };
        // The batch then ends with "Query cancelled".
        runtime::spawn_detached(async move { cancel.cancel().await });
        cx.notify();
    }

    fn show_active(&mut self, cx: &mut Context<Self>) {
        let res = self.results.get(self.active).cloned().unwrap_or_default();
        self.grid.update(cx, |t, cx| {
            let g = t.delegate_mut();
            g.sort = None;
            g.set_result(&res);
            g.editable.clear();
            t.refresh(cx);
            cx.notify();
        });
        self.detect_editable(&res, cx);
    }

    /// A result from a single-table SELECT that includes the primary key
    /// can be edited like a table (as in TablePlus).
    fn detect_editable(&mut self, res: &QueryResult, cx: &mut Context<Self>) {
        self.edit_gen += 1;
        self.target = None;
        self.edit_note = None;
        let driver = self.shared.cfg.driver;
        if res.columns.is_empty()
            || !matches!(driver, Driver::Postgres | Driver::MySql | Driver::Sqlite)
        {
            return;
        }
        let Some((schema, name)) = crate::db::target::single_table(&res.statement, driver) else {
            self.edit_note = Some("Read-only: only single-table SELECTs can be edited".into());
            return;
        };
        let schema = schema.or_else(|| match driver {
            Driver::Postgres => Some(
                self.shared
                    .conn
                    .try_read()
                    .ok()
                    .and_then(|c| c.current_schema())
                    .unwrap_or_else(|| "public".into()),
            ),
            _ => None,
        });
        let obj = DbObject {
            schema,
            name,
            kind: ObjectKind::Table,
        };
        let conn = self.shared.conn.clone();
        let o = obj.clone();
        let task = runtime::spawn(async move { conn.read().await.structure(&o).await });
        let (generation, result_cols) = (self.edit_gen, res.columns.clone());
        cx.spawn(async move |this, cx| {
            let st = task.await;
            this.update(cx, |this, cx| {
                if generation != this.edit_gen {
                    return;
                }
                let cols = match st {
                    Ok(s) => s.columns,
                    Err(e) => {
                        this.edit_note = Some(format!("Read-only: couldn't check {} ({e:#})", obj.title()));
                        cx.notify();
                        return;
                    }
                };
                match map_columns(&result_cols, &obj.title(), &cols) {
                    Ok((editable, pk)) => {
                        this.grid.update(cx, |t, cx| {
                            let g = t.delegate_mut();
                            g.editable = editable;
                            g.pk = pk;
                            cx.notify();
                        });
                        this.target = Some(EditTarget { obj, columns: cols });
                    }
                    Err(note) => this.edit_note = Some(note),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    #[cfg(test)]
    pub fn run_for_test(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.execute(text.into(), window, cx);
    }

    #[cfg(test)]
    pub fn is_editable(&self) -> bool {
        self.target.is_some()
    }

    #[cfg(test)]
    pub fn note(&self) -> Option<String> {
        self.edit_note.clone()
    }

    #[cfg(test)]
    pub fn cell(&self, r: usize, c: usize, cx: &App) -> Option<String> {
        self.grid.read(cx).delegate().cell_string(r, c)
    }

    #[cfg(test)]
    pub fn select_cell(&mut self, r: usize, c: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.grid.update(cx, |t, cx| t.set_selected_cell(r, c, cx));
        let fh = self.grid.focus_handle(cx);
        window.focus(&fh, cx);
    }

    /// The editor's current text.
    pub fn text_now(&self, cx: &App) -> String {
        self.editor.read(cx).value().to_string()
    }

    #[cfg(test)]
    pub fn plan_labels(&self) -> Vec<String> {
        self.plan
            .iter()
            .flat_map(|p| p.flatten().into_iter().map(|(_, n)| n.label.clone()))
            .collect()
    }

    #[cfg(test)]
    pub fn text(&self, cx: &App) -> String {
        self.editor.read(cx).value().to_string()
    }

    #[cfg(test)]
    pub fn running(&self) -> bool {
        self.running
    }

    pub fn is_dirty(&self, cx: &App) -> bool {
        self.grid.read(cx).delegate().is_dirty()
    }

    /// Re-running or switching results would drop staged edits.
    fn blocked_by_edits(&self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.is_dirty(cx) {
            window.push_notification(
                Notification::warning("Commit (⌘S) or discard (⌘⇧⌫) your changes first"),
                cx,
            );
            return true;
        }
        false
    }

    pub fn discard(&mut self, cx: &mut Context<Self>) {
        self.grid.update(cx, |t, cx| {
            t.delegate_mut().discard();
            cx.notify();
        });
        cx.notify();
    }

    fn preview(&self, cx: &App) -> String {
        let Some(t) = &self.target else {
            return String::new();
        };
        let changes = self.grid.read(cx).delegate().changes();
        self.shared
            .conn
            .try_read()
            .map(|c| c.preview(&t.obj, &t.columns, &changes))
            .unwrap_or_else(|_| "(The connection is busy; the statements will be shown when it's free.)".into())
    }

    pub fn commit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        cell_edit::finish_edit(self, window, cx);
        if self.target.is_none() || !self.is_dirty(cx) {
            return;
        }
        let gate = crate::db::safety::gate_write(&self.shared.cfg);
        let weak = cx.entity().downgrade();
        let preview = self.preview(cx);
        if super::guarded(gate, &self.shared.cfg, "Commit", preview, window, cx, move |w, cx| {
            weak.update(cx, |t, cx| t.do_commit(w, cx)).ok();
        }) {
            self.do_commit(window, cx);
        }
    }

    fn do_commit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(t) = self.target.clone() else { return };
        if self.committing {
            return;
        }
        self.committing = true;
        let changes = self.grid.read(cx).delegate().changes();
        let n = changes.len();
        let conn = self.shared.conn.clone();
        let task =
            runtime::spawn(
                async move { conn.read().await.commit(&t.obj, &t.columns, &changes).await },
            );
        cx.spawn_in(window, async move |this, cx| {
            let res = task.await;
            this.update_in(cx, |this, window, cx| {
                this.committing = false;
                match res {
                Ok(()) => {
                    window.push_notification(
                        Notification::success(format!("Committed {n} change(s)")),
                        cx,
                    );
                    // Show what's saved now.
                    this.discard(cx);
                    let text = this.last_run.clone();
                    this.execute(text, window, cx);
                }
                Err(e) => window.push_notification(
                    Notification::error(format!("{e:#}")).title("Commit failed"),
                    cx,
                ),
                }
            })
            .ok();
        })
        .detach();
    }

    fn render_results(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let current = self.results.get(self.active);
        let summary = match current {
            _ if self.running => "Running…".to_string(),
            _ if self.plan.is_some() => "Query plan · bars show each step's share of the cost / time".to_string(),
            None => "⌘↵ to run".to_string(),
            Some(r) if r.columns.is_empty() => {
                format!(
                    "{} row(s) affected · {} ms",
                    r.affected.unwrap_or(0),
                    r.elapsed_ms
                )
            }
            Some(r) => format!("{} row(s) · {} ms", r.rows.len(), r.elapsed_ms),
        };
        let body = if let (Some(p), None) = (&self.plan, &self.error) {
            self.render_plan(p, cx)
        } else if let Some(e) = &self.error {
            div()
                .size_full()
                .p_3()
                .text_sm()
                .text_color(theme.danger)
                .font_family(theme.mono_font_family.clone())
                .child(e.clone())
                .into_any_element()
        } else if current.is_some_and(|r| r.columns.is_empty()) {
            div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .text_color(theme.muted_foreground)
                .child(summary.clone())
                .into_any_element()
        } else {
            let grid = cell_edit::editable(data_table(&self.grid, cx), cx);
            if self.show_detail {
                let types: Vec<Option<String>> = {
                    let cols = &self.grid.read(cx).delegate().columns;
                    cols.iter()
                        .map(|c| {
                            let t = self.target.as_ref()?;
                            t.columns.iter().find(|i| &i.name == c).map(|i| i.data_type.clone())
                        })
                        .collect()
                };
                h_flex()
                    .size_full()
                    .child(div().flex_1().min_w_0().h_full().child(grid))
                    .child(row_panel::render(self, &types, cx))
                    .into_any_element()
            } else {
                grid.into_any_element()
            }
        };
        v_flex()
            .size_full()
            .when(self.results.len() > 1, |d| {
                d.child(
                    TabBar::new("results")
                        .underline()
                        .small()
                        .selected_index(self.active)
                        .children(self.results.iter().enumerate().map(|(i, r)| {
                            let first = r
                                .statement
                                .lines()
                                .next()
                                .unwrap_or("")
                                .chars()
                                .take(28)
                                .collect::<String>();
                            Tab::new().label(format!("{}: {first}", i + 1))
                        }))
                        .on_click(cx.listener(|this, ix: &usize, window, cx| {
                            if *ix == this.active || this.blocked_by_edits(window, cx) {
                                return;
                            }
                            this.active = *ix;
                            this.show_active(cx);
                            cx.notify();
                        })),
                )
            })
            .child(div().flex_1().min_h_0().child(body))
            .child(
                h_flex()
                    .h(rems(1.875))
                    .px_2()
                    .border_t_1()
                    .border_color(theme.border)
                    .bg(theme.title_bar)
                    .gap_1()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(summary)
                    .child(div().flex_1())
                    .child(Button::new("toggle-detail")
                    .icon(IconName::PanelRight)
                    .ghost()
                    .xsmall()
                    .selected(self.show_detail)
                    .tooltip("Row detail (⌘⇧I)")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.show_detail = !this.show_detail;
                        cx.notify();
                    })))
                    .map(|bar| {
                        let pending = self.grid.read(cx).delegate().change_count();
                        if pending > 0 {
                            bar.child(
                                Button::new("discard-result")
                                    .label("Discard")
                                    .ghost()
                                    .xsmall()
                                    .on_click(cx.listener(|this, _, _, cx| this.discard(cx))),
                            )
                            .child(
                                Button::new("preview-result")
                                    .label("Preview SQL")
                                    .ghost()
                                    .xsmall()
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        let sql: SharedString = this.preview(cx).into();
                                        window.open_alert_dialog(cx, move |d, _, _| {
                                            d.title("Pending changes").description(sql.clone())
                                        });
                                    })),
                            )
                            .child(
                                Button::new("commit-result")
                                    .label(format!("Commit {pending}"))
                                    .primary()
                                    .xsmall()
                                    .tooltip("Commit (⌘S)")
                                    .on_click(
                                        cx.listener(|this, _, window, cx| this.commit(window, cx)),
                                    ),
                            )
                        } else {
                            bar.when_some(self.edit_note.clone(), |b, note| b.child(note))
                                .when(self.target.is_some(), |b| {
                                    b.child("Editable · Enter or type to edit a cell")
                                })
                        }
                    }),
            )
    }
}

impl copy::ExportHost for QueryTab {
    fn export_grid(&self) -> &Entity<TableState<Grid>> {
        &self.grid
    }

    /// Editable results write back to their table; others to a placeholder.
    fn sql_target(&self, _cx: &App) -> crate::export::SqlTarget {
        let (schema, table) = match &self.target {
            Some(t) => (t.obj.schema.clone(), t.obj.name.clone()),
            None => (None, "query_result".into()),
        };
        crate::export::SqlTarget {
            driver: self.shared.cfg.driver,
            schema,
            table,
        }
    }

    fn export_name(&self) -> String {
        match &self.target {
            Some(t) => t.obj.name.clone(),
            None => "query_result".into(),
        }
    }

    fn export_source(&self, _cx: &App) -> copy::Source {
        copy::Source::Loaded
    }
}

impl cell_edit::CellEditHost for QueryTab {
    fn grid(&self) -> &Entity<TableState<Grid>> {
        &self.grid
    }

    fn edit_sub(&mut self) -> &mut Option<Subscription> {
        &mut self.edit_sub
    }
}

impl Focusable for QueryTab {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for QueryTab {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let toolbar = h_flex()
            .h(rems(2.25))
            .px_2()
            .gap_1()
            .border_b_1()
            .border_color(theme.border)
            .child(if self.running {
                Button::new("stop")
                    .icon(IconName::Square)
                    .label("Stop")
                    .danger()
                    .small()
                    .tooltip("Cancel the running query (⌘.)")
                    .on_click(cx.listener(|this, _, _, cx| this.cancel(cx)))
            } else {
                Button::new("run")
                    .icon(IconName::Play)
                    .label("Run")
                    .primary()
                    .small()
                    .tooltip("Run selection or all (⌘↵)")
                    .on_click(cx.listener(|this, _, window, cx| this.run(&RunQuery, window, cx)))
            })
            .when(
                matches!(self.shared.cfg.driver, Driver::Postgres | Driver::MySql | Driver::Sqlite),
                |d| {
                    d.child(
                        Button::new("explain")
                            .label("Explain")
                            .ghost()
                            .small()
                            .tooltip("Show the query plan (⌘E)")
                            .on_click(cx.listener(|this, _, window, cx| this.explain(false, window, cx))),
                    )
                    .when(self.shared.cfg.driver != Driver::Sqlite, |d| {
                        d.child(
                            Button::new("analyze")
                                .label("Analyze")
                                .ghost()
                                .small()
                                .tooltip("Run it and show actual times (rolled back afterwards)")
                                .on_click(
                                    cx.listener(|this, _, window, cx| this.explain(true, window, cx)),
                                ),
                        )
                    })
                },
            )
            .child(
                Button::new("save-query")
                    .label("Save")
                    .ghost()
                    .small()
                    .tooltip("Save query (⌘⇧S)")
                    .on_click(cx.listener(|this, _, window, cx| this.save(window, cx))),
            )
            .when(
                !matches!(self.shared.cfg.driver, Driver::Redis | Driver::Mongo),
                |d| {
                    d.child(
                        Button::new("format")
                            .label("Format")
                            .ghost()
                            .small()
                            .tooltip("Format SQL (⌘⇧F)")
                            .on_click(cx.listener(|this, _, window, cx| this.format(window, cx))),
                    )
                },
            )
            .child(div().text_xs().text_color(theme.muted_foreground).child(
                match self.shared.cfg.driver {
                    Driver::Redis => "One command per line",
                    Driver::Mongo => {
                        "mongosh syntax: db.coll.find({...}), aggregate([...]), or a {command}"
                    }
                    _ => "Separate statements with ;",
                },
            ));
        v_flex()
            .size_full()
            .key_context("QueryTab")
            .track_focus(&self.focus)
            .child(toolbar)
            .child(
                v_resizable("query-split")
                    .child(
                        resizable_panel().child(Editor::new(&self.editor).bordered(false).h_full()),
                    )
                    .child(
                        resizable_panel()
                            .size(px(320.))
                            .child(self.render_results(cx)),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn col(name: &str, pk: bool) -> ColumnInfo {
        ColumnInfo {
            name: name.into(),
            data_type: "text".into(),
            nullable: true,
            default: None,
            is_pk: pk,
        }
    }

    #[test]
    fn maps_result_columns_to_the_table() {
        let table = [col("id", true), col("a", false), col("b", false)];
        let cols = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        // Real columns are editable; computed and repeated ones aren't.
        let (editable, pk) = map_columns(&cols(&["id", "a", "n", "a"]), "t", &table).unwrap();
        assert_eq!(editable, [true, true, false, false]);
        assert_eq!(pk, [true, false, false, false]);
        assert!(
            map_columns(&cols(&["a"]), "t", &table)
                .unwrap_err()
                .contains("primary key (id)")
        );
        assert!(
            map_columns(&cols(&["a"]), "t", &[col("a", false)])
                .unwrap_err()
                .contains("no primary key")
        );
        assert!(
            map_columns(&cols(&["a"]), "t", &[])
                .unwrap_err()
                .contains("couldn't find")
        );
    }
}
