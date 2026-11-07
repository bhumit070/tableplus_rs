//! SQL / command editor with a results pane. ⌘↵ runs the selection, or the
//! whole editor when nothing is selected. Each statement gets a result tab.

use std::rc::Rc;
use std::sync::Arc;
use std::time::Instant;

use gpui::{
    App, ClipboardItem, Context, Entity, FocusHandle, Focusable, IntoElement, ParentElement,
    Render, SharedString, Styled, Subscription, Window, div, prelude::*, px,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::dialog::DialogFooter;
use gpui_component::input::{Editor, EditorState, Input, InputState, Position};
use gpui_component::menu::DropdownMenu as _;
use gpui_component::menu::PopupMenuItem;
use gpui_component::notification::Notification;
use gpui_component::switch::Switch;
use gpui_component::table::{TableEvent, TableState};
use gpui_component::{
    ActiveTheme as _, Icon, Selectable as _, Sizable as _, WindowExt as _, h_flex, resizable_panel,
    v_flex, v_resizable,
};
use gpui_kit_assets::IconName;

use super::cell_edit::{self, EditStart};
use super::copy;
use super::dialog;
use super::grid::{Grid, data_table};
use super::loading;
use super::row_panel;
use super::theme::{dp, kbd, tokens};
use super::{DisabledWhen as _, QueryHistory, RunQuery, Shared, SwitchDatabase};
use crate::db::explain::{self, PlanNode};
use crate::db::split::{split_statements, statement_at};
use crate::db::target::with_limit;
use crate::db::{BatchFailed, Cancel, ColumnInfo, DbObject, Driver, ObjectKind, QueryResult};
use crate::{history, runtime};
use anyhow::Error;
use gpui::{AnyElement, FontWeight, Hsla, relative, transparent_black};
use lsp_types::{Diagnostic as LspDiagnostic, DiagnosticSeverity, Range as LspRange};
use uuid::Uuid;

pub struct QueryTab {
    shared: Rc<Shared>,
    pub title: String,
    editor: Entity<EditorState>,
    results: Vec<QueryResult>,
    active: usize,
    grid: Entity<TableState<Grid>>,
    running: bool,
    /// When the running batch or EXPLAIN began.
    run_started: Option<Instant>,
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
    plan: Option<PlanNode>,
    /// The server's answer for `plan`, for the Raw view and Copy plan.
    plan_raw: String,
    /// Whether `plan` came from EXPLAIN ANALYZE.
    plan_analyzed: bool,
    /// Plan shown as text instead of the tree.
    plan_as_raw: bool,
    /// Set once this tab's text is a saved query; ⌘⇧S then updates it.
    saved: Option<crate::saved::SavedQuery>,
    /// Rows a plain SELECT is limited to (toolbar "Limit"); `None` = all.
    limit: Option<usize>,
    /// With `error`: the statement that failed, for its tab.
    error_stmt: Option<String>,
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

/// The editor position (line, UTF-16 column) of byte `offset` in `text`.
fn position(text: &str, offset: usize) -> Position {
    let before = &text[..offset.min(text.len())];
    let line = before.matches('\n').count() as u32;
    let line_start = before.rfind('\n').map(|i| i + 1).unwrap_or(0);
    let character = before[line_start..].encode_utf16().count() as u32;
    Position { line, character }
}

/// Toolbar "Limit" choices.
const LIMITS: [Option<usize>; 5] = [None, Some(100), Some(500), Some(1000), Some(5000)];

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
        grid.on_menu = Some(Rc::new(move |row, menu, window, cx| {
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
            .map(|menu| copy::copy_items(menu, weak.clone(), row, window, cx))
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
            run_started: None,
            cancel: None,
            show_detail: false,
            saved: None,
            limit: None,
            error_stmt: None,
            plan: None,
            plan_raw: String::new(),
            plan_analyzed: false,
            plan_as_raw: false,
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

    /// ⌘↵: the selection, or else the statement the cursor is in (each
    /// statement gets a result tab, so running one gives one).
    pub fn run(&mut self, _: &RunQuery, window: &mut Window, cx: &mut Context<Self>) {
        let driver = self.shared.cfg.driver;
        let e = self.editor.read(cx);
        let sel = e.selected_value();
        let text = if !sel.trim().is_empty() {
            sel.to_string()
        } else if matches!(driver, Driver::Postgres | Driver::MySql | Driver::Sqlite) {
            let all = e.value().to_string();
            statement_at(&all, e.cursor(), driver == Driver::MySql).unwrap_or(all)
        } else {
            // Redis and MongoDB: the editor as written.
            e.value().to_string()
        };
        self.run_text(text, window, cx);
    }

    /// ⌘⇧↵: everything in the editor (or the selection), one tab per
    /// statement.
    pub fn run_all(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let e = self.editor.read(cx);
        let sel = e.selected_value();
        let text = if sel.trim().is_empty() {
            e.value().to_string()
        } else {
            sel.to_string()
        };
        self.run_text(text, window, cx);
    }

    fn run_text(&mut self, text: String, window: &mut Window, cx: &mut Context<Self>) {
        if self.running {
            return;
        }
        if text.trim().is_empty() {
            return;
        }
        if self.blocked_by_edits(window, cx) {
            return;
        }
        let gate = crate::db::safety::gate_query(&self.shared.cfg, &text);
        let weak = cx.entity().downgrade();
        let t2 = text.clone();
        if super::guarded(
            gate,
            &self.shared.cfg,
            "Run",
            text.clone(),
            window,
            cx,
            move |w, cx| {
                weak.update(cx, |t, cx| t.execute(t2.clone(), w, cx)).ok();
            },
        ) {
            self.execute(text, window, cx);
        }
    }

    /// ⌘E: the plan of the selected / first statement. `analyze` runs it
    /// (inside a transaction that's rolled back, so writes don't stick).
    pub fn explain(&mut self, analyze: bool, window: &mut Window, cx: &mut Context<Self>) {
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
        let text = if sel.trim().is_empty() {
            e.value().to_string()
        } else {
            sel
        };
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
            if !super::guarded(
                gate,
                &self.shared.cfg,
                "Run EXPLAIN ANALYZE",
                stmt.clone(),
                window,
                cx,
                move |w, cx| {
                    weak.update(cx, |t, cx| t.run_explain(w2.clone(), true, w, cx))
                        .ok();
                },
            ) {
                return;
            }
        }
        self.run_explain(wrapped, analyze, window, cx);
    }

    fn run_explain(
        &mut self,
        sql: String,
        analyze: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let driver = self.shared.cfg.driver;
        self.start_running(cx);
        self.error = None;
        cx.notify();
        let conn = self.shared.conn.clone();
        let cancel = Arc::new(Cancel::default());
        self.cancel = Some(cancel.clone());
        let task = runtime::spawn(async move {
            let rs = conn.read().await.execute_cancellable(&sql, &cancel).await?;
            let r = rs
                .iter()
                .find(|r| {
                    r.statement
                        .trim_start()
                        .to_ascii_uppercase()
                        .starts_with("EXPLAIN")
                })
                .cloned()
                .unwrap_or_default();
            Ok((explain::parse(driver, &r)?, explain::raw_text(driver, &r)))
        });
        cx.spawn_in(window, async move |this, cx| {
            let res = task.await;
            this.update(cx, |this, cx| {
                this.running = false;
                this.cancel = None;
                match res {
                    Ok((p, raw)) => {
                        this.plan = Some(p);
                        this.plan_raw = raw;
                        this.plan_analyzed = analyze;
                    }
                    Err(e) => this.error = Some(format!("{e:#}")),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn render_plan(&self, plan: &PlanNode, cx: &mut Context<Self>) -> AnyElement {
        let t = tokens(cx);
        let mono = cx.theme().mono_font_family.clone();
        let driver = self.shared.cfg.driver;
        let me = cx.entity();
        let header =
            h_flex()
                .h(dp(40.))
                .flex_none()
                .px(dp(10.))
                .gap(dp(10.))
                .border_b_1()
                .border_color(t.border)
                .child(
                    dialog::segmented(
                        "plan-view",
                        &["Tree", "Raw"],
                        self.plan_as_raw as usize,
                        Rc::new({
                            let me = me.clone();
                            move |i, _, cx| {
                                me.update(cx, |this, cx| {
                                    this.plan_as_raw = i == 1;
                                    cx.notify();
                                })
                            }
                        }),
                        cx,
                    )
                    .w(dp(140.)),
                )
                .when(driver != Driver::Sqlite, |d| {
                    d.child(
                        Switch::new("plan-analyze")
                            .checked(self.plan_analyzed)
                            .label("Analyze")
                            .tooltip("Run it for real times and rows (rolled back afterwards)")
                            .on_click(cx.listener(|this, on: &bool, window, cx| {
                                this.explain(*on, window, cx)
                            })),
                    )
                })
                .child(div().flex_1())
                .children(plan.details.iter().filter(|d| d.ends_with(" ms")).map(|d| {
                    div()
                        .text_size(dp(11.5))
                        .text_color(t.text_muted)
                        .child(d.clone())
                }))
                .child(
                    Button::new("copy-plan")
                        .cursor_pointer()
                        .icon(IconName::Copy)
                        .label("Copy plan")
                        .ghost()
                        .xsmall()
                        .on_click(cx.listener(|this, _, window, cx| {
                            cx.write_to_clipboard(ClipboardItem::new_string(this.plan_raw.clone()));
                            window.push_notification("Plan copied", cx);
                        })),
                );
        if self.plan_as_raw {
            return v_flex()
                .size_full()
                .child(header)
                .child(
                    div()
                        .id("plan-raw")
                        .flex_1()
                        .min_h_0()
                        .overflow_y_scroll()
                        .p(dp(12.))
                        .font_family(mono)
                        .text_size(dp(12.))
                        .text_color(t.text_secondary)
                        .children(self.plan_raw.lines().map(|l| {
                            div().whitespace_nowrap().child(if l.is_empty() {
                                " ".to_string()
                            } else {
                                l.to_string()
                            })
                        })),
                )
                .into_any_element();
        }

        let nodes = plan.flatten();
        let total = plan
            .actual_ms
            .or(plan.cost)
            .unwrap_or(0.0)
            .max(f64::EPSILON);
        let hottest = nodes
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.1.self_weight().total_cmp(&b.1.1.self_weight()))
            .map(|(i, _)| i);
        let num = |v: f64| {
            if v >= 100.0 {
                group(v.round() as u64)
            } else if v.fract() == 0.0 {
                format!("{v:.0}")
            } else {
                format!("{v:.2}")
            }
        };
        let col = |w: f32| div().flex_none().w(dp(w));
        // SQLite's plan has steps but no numbers: no bars then.
        let costed = nodes
            .iter()
            .any(|(_, n)| n.cost.is_some() || n.actual_ms.is_some());
        let head = h_flex()
            .h(dp(30.))
            .px(dp(12.))
            .gap(dp(12.))
            .border_b_1()
            .border_color(t.border)
            .bg(t.sidebar)
            .text_size(dp(11.))
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(t.text_muted)
            .child(div().flex_1().child("OPERATION"))
            .when(costed, |d| {
                d.child(col(130.).child(if self.plan_analyzed {
                    "OWN TIME"
                } else {
                    "OWN COST"
                }))
                .child(col(150.).child("SELF · WITH CHILDREN"))
                .child(col(150.).child("ROWS  EST → ACTUAL"))
            })
            .when(self.plan_analyzed, |d| d.child(col(80.).child("TIME")));
        let rows = nodes.iter().enumerate().map(|(i, (depth, n))| {
            let own = n.self_weight() / total;
            let incl = n.actual_ms.or(n.cost).unwrap_or(0.0) / total;
            let bar = if Some(i) == hottest && own > 0.0 {
                t.danger
            } else if own > 0.3 {
                t.warning
            } else {
                t.accent
            };
            let rows_text = match (n.rows, n.actual_rows) {
                (Some(e), Some(a)) => {
                    let off = (e.max(1.0) / a.max(1.0)).max(a.max(1.0) / e.max(1.0)) > 10.0;
                    div().when(off, |d| d.text_color(t.warning)).child(format!(
                        "{} → {}",
                        num(e),
                        num(a)
                    ))
                }
                (Some(e), None) => div().child(num(e)),
                (None, Some(a)) => div().child(format!("→ {}", num(a))),
                (None, None) => div().text_color(t.text_faint).child("—"),
            };
            h_flex()
                .px(dp(12.))
                .py(dp(7.))
                .gap(dp(12.))
                .items_start()
                .border_b_1()
                .border_color(t.border.opacity(0.6))
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .pl(dp(22. * *depth as f32))
                        .gap(dp(2.))
                        .child(
                            div()
                                .text_size(dp(12.5))
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(t.text)
                                .child(n.label.clone()),
                        )
                        .children(n.details.iter().filter(|d| !d.ends_with(" ms")).map(|d| {
                            div()
                                .font_family(mono.clone())
                                .text_size(dp(11.5))
                                .text_color(t.text_muted)
                                .child(d.clone())
                        })),
                )
                .when(costed, |d| {
                    d.child(
                        col(130.).pt(dp(5.)).child(
                            div().h(dp(6.)).w_full().rounded_full().bg(t.hover).child(
                                div()
                                    .h_full()
                                    .rounded_full()
                                    .bg(bar)
                                    .w(relative(own.clamp(0.0, 1.0) as f32)),
                            ),
                        ),
                    )
                    .child(
                        col(150.)
                            .text_size(dp(11.5))
                            .text_color(t.text_secondary)
                            .child(format!(
                                "self {:.0}% · total {:.0}%",
                                own * 100.0,
                                incl * 100.0
                            )),
                    )
                    .child(
                        col(150.)
                            .font_family(mono.clone())
                            .text_size(dp(11.5))
                            .text_color(t.text_secondary)
                            .child(rows_text),
                    )
                })
                .when(self.plan_analyzed, |d| {
                    d.child(
                        col(80.)
                            .font_family(mono.clone())
                            .text_size(dp(11.5))
                            .text_color(t.text_secondary)
                            .child(
                                n.actual_ms
                                    .map(|ms| format!("{} ms", num(ms)))
                                    .unwrap_or_default(),
                            ),
                    )
                })
        });
        let tip = explain::insight(plan).map(|i| {
            let sql = i.sql.clone();
            h_flex()
                .m(dp(10.))
                .px(dp(12.))
                .py(dp(10.))
                .gap(dp(10.))
                .rounded(dp(8.))
                .border_1()
                .border_color(t.accent.opacity(0.35))
                .bg(t.accent.opacity(0.08))
                .child(Icon::new(IconName::Lightbulb).small().text_color(t.accent))
                .child(
                    div()
                        .flex_1()
                        .text_size(dp(12.5))
                        .text_color(t.text)
                        .child(i.text),
                )
                .when_some(sql, |d, sql| {
                    d.child(
                        Button::new("copy-index")
                            .cursor_pointer()
                            .icon(IconName::Copy)
                            .label("Copy CREATE INDEX")
                            .outline()
                            .xsmall()
                            .on_click(move |_, window, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(sql.clone()));
                                window.push_notification("Copied", cx);
                            }),
                    )
                })
        });
        v_flex()
            .size_full()
            .child(header)
            .children(tip)
            .child(head)
            .child(
                div()
                    .id("plan")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(v_flex().children(rows)),
            )
            .into_any_element()
    }

    fn execute(&mut self, text: String, window: &mut Window, cx: &mut Context<Self>) {
        self.plan = None;
        self.last_run = text.clone();
        self.start_running(cx);
        self.error = None;
        self.error_stmt = None;
        self.mark_failed(None, cx);
        cx.notify();
        let driver = self.shared.cfg.driver;
        let stmts = match driver {
            Driver::Postgres | Driver::MySql | Driver::Sqlite => {
                split_statements(&text, driver == Driver::MySql)
            }
            _ => vec![],
        };
        // The toolbar limit: added to plain SELECTs that have none. History
        // keeps the text as typed.
        let to_run = match self.limit {
            Some(n) if !stmts.is_empty() => stmts
                .iter()
                .map(|s| with_limit(s, n).unwrap_or_else(|| s.clone()))
                .collect::<Vec<_>>()
                .join(";\n"),
            _ => text.clone(),
        };
        let conn = self.shared.conn.clone();
        let start = Instant::now();
        let cancel = Arc::new(Cancel::default());
        self.cancel = Some(cancel.clone());
        let conn_id = self.shared.cfg.id.clone();
        let task = runtime::spawn(async move {
            let c = conn.read().await;
            let ran_at = history::now();
            let res = c.execute_cancellable(&to_run, &cancel).await;
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
                error: res.as_ref().err().map(error_text),
            };
            drop(c);
            // History is a convenience: never fail or delay the run over it.
            runtime::spawn_detached(async move {
                if let Err(e) = history::record(entry).await {
                    eprintln!("query history: {e:#}");
                }
            });
            res
        });
        cx.spawn_in(window, async move |this, cx| {
            let res = task.await;
            this.update_in(cx, |this, _, cx| {
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
                        this.error = Some(error_text(&e));
                        let cancelled = e
                            .root_cause()
                            .to_string()
                            .starts_with(crate::db::cancel::CANCELLED);
                        // What ran before the failing statement stays
                        // visible, next to the error's own tab.
                        let failed = e.downcast_ref::<BatchFailed>().cloned();
                        let index = failed.as_ref().map(|f| f.index);
                        this.results = failed.map(|f| f.done).unwrap_or_default();
                        this.error_stmt = index.and_then(|i| stmts.get(i).cloned());
                        this.active = this.results.len();
                        this.show_active(cx);
                        if !cancelled {
                            this.mark_failed(index, cx);
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
    /// Inserts `text` at the editor's cursor and focuses the editor.
    pub fn insert_text(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        let text = text.to_string();
        self.editor.update(cx, |e, cx| {
            e.insert(text, window, cx);
            e.focus(window, cx);
        });
    }

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
                        id: Uuid::new_v4().to_string(),
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
                                .cursor_pointer()
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
                                .cursor_pointer()
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
    /// A result tab's ×: drops that result (the error tab is the one past
    /// the results). Not while its edits are staged.
    fn close_result(&mut self, i: usize, window: &mut Window, cx: &mut Context<Self>) {
        if i == self.active && self.blocked_by_edits(window, cx) {
            return;
        }
        if i < self.results.len() {
            self.results.remove(i);
        } else {
            self.error = None;
            self.error_stmt = None;
            self.mark_failed(None, cx);
        }
        let shown = self.results.len() + self.error.is_some() as usize;
        if self.active > i || self.active >= shown {
            self.active = self.active.saturating_sub(1);
        }
        self.show_active(cx);
        cx.notify();
    }

    /// Marks the tab busy and keeps the loading chip's clock ticking.
    fn start_running(&mut self, cx: &mut Context<Self>) {
        self.running = true;
        self.run_started = Some(Instant::now());
        loading::tick(cx, |q: &Self| q.running);
    }

    /// Underlines statement `index` of the last run in the editor (or
    /// clears the mark). Only when that text is still in the editor.
    fn mark_failed(&mut self, index: Option<usize>, cx: &mut Context<Self>) {
        let driver = self.shared.cfg.driver;
        let full = self.editor.read(cx).value().to_string();
        let range = index.and_then(|i| {
            let base = full.find(&self.last_run)?;
            let stmts = split_statements(&self.last_run, driver == Driver::MySql);
            let mut from = 0;
            let mut found = None;
            for (n, stmt) in stmts.iter().enumerate() {
                let t = stmt.trim();
                let at = self.last_run[from..].find(t)? + from;
                if n == i {
                    found = Some((base + at, base + at + t.len()));
                    break;
                }
                from = at + t.len();
            }
            found
        });
        let message = self.error.clone().unwrap_or_default();
        self.editor.update(cx, |e, cx| {
            if let Some(d) = e.diagnostics_mut() {
                d.clear();
                if let Some((a, b)) = range {
                    d.push(LspDiagnostic {
                        range: LspRange::new(position(&full, a), position(&full, b)),
                        severity: Some(DiagnosticSeverity::ERROR),
                        message: message.clone(),
                        ..Default::default()
                    });
                }
            }
            cx.notify();
        });
    }

    /// "Ln 3, Col 14" for the status bar.
    fn cursor_label(&self, cx: &App) -> String {
        let p = self.editor.read(cx).cursor_position();
        format!("Ln {}, Col {}", p.line + 1, p.character + 1)
    }

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
                        this.edit_note =
                            Some(format!("Read-only: couldn't check {} ({e:#})", obj.title()));
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
                            g.set_choices(&cols);
                            g.set_types(&cols, &[]);
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
    pub fn result_count(&self) -> usize {
        self.results.len()
    }

    /// Puts `text` in the editor and runs it, as ⌘↵ would.
    #[cfg(test)]
    pub fn type_and_run(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.editor
            .update(cx, |e, cx| e.set_value(text.to_string(), window, cx));
        self.execute(text.into(), window, cx);
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

    /// The connection switched databases: staged edits and the edit target
    /// belong to the old one, so drop them and make the result read-only.
    pub fn database_changed(&mut self, cx: &mut Context<Self>) {
        self.edit_gen += 1;
        self.target = None;
        self.grid.update(cx, |t, cx| {
            let g = t.delegate_mut();
            g.discard();
            g.editable.clear();
            cx.notify();
        });
        if !self.results.is_empty() {
            self.edit_note =
                Some("Read-only: the database changed; run the query again to edit".into());
        }
        cx.notify();
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
            .unwrap_or_else(|_| {
                "(The connection is busy; the statements will be shown when it's free.)".into()
            })
    }

    pub fn commit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        cell_edit::finish_edit(self, window, cx);
        if self.target.is_none() || !self.is_dirty(cx) {
            return;
        }
        let gate = crate::db::safety::gate_write(&self.shared.cfg);
        let weak = cx.entity().downgrade();
        let preview = self.preview(cx);
        if super::guarded(
            gate,
            &self.shared.cfg,
            "Commit",
            preview,
            window,
            cx,
            move |w, cx| {
                weak.update(cx, |t, cx| t.do_commit(w, cx)).ok();
            },
        ) {
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
                        // Show what's saved now: re-run only the edited result's
                        // statement, and only when it just reads. Re-running the
                        // whole batch would repeat its writes without the
                        // safe-mode check.
                        this.discard(cx);
                        let stmt = this.results.get(this.active).map(|r| r.statement.clone());
                        if let Some(stmt) = stmt
                            .filter(|s| crate::db::safety::is_read_only(this.shared.cfg.driver, s))
                        {
                            this.execute(stmt, window, cx);
                        }
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

    /// For the status bar: "Result 1 · 6 rows · 42 ms", once something ran.
    pub fn status_line(&self, cx: &App) -> String {
        let cursor = self.cursor_label(cx);
        match self.result_line() {
            Some(r) => format!("{r}  ·  {cursor}"),
            None => cursor,
        }
    }

    fn result_line(&self) -> Option<String> {
        if self.running {
            return Some("Running…".into());
        }
        let r = self.results.get(self.active)?;
        let what = if r.columns.is_empty() {
            format!("{} affected", r.affected.unwrap_or(0))
        } else if r.rows.len() == 1 {
            "1 row".to_string()
        } else {
            format!("{} rows", r.rows.len())
        };
        Some(format!(
            "Result {} · {what} · {} ms",
            self.active + 1,
            r.elapsed_ms
        ))
    }

    /// One tab per statement result (status dot, rows · time), plus the
    /// failing statement's tab when a run stopped on an error.
    fn render_result_tabs(&self, cx: &mut Context<Self>) -> AnyElement {
        let t = tokens(cx);
        let mut tabs: Vec<AnyElement> = vec![];
        let tab = |i: usize, dot: Hsla, label: String, meta: String, cx: &mut Context<Self>| {
            let active = i == self.active;
            h_flex()
                .id(("result-tab", i))
                .debug_selector(move || format!("result-tab-{i}"))
                .h_full()
                .px(dp(12.))
                .gap(dp(8.))
                .flex_none()
                .max_w(dp(280.))
                .cursor_pointer()
                .border_b_2()
                .border_color(if active {
                    t.accent
                } else {
                    transparent_black()
                })
                .text_size(dp(12.))
                .text_color(if active { t.text } else { t.text_secondary })
                .hover(|s| s.text_color(t.text))
                .child(div().flex_none().size(dp(6.)).rounded_full().bg(dot))
                .child(
                    div()
                        .truncate()
                        .font_family(cx.theme().mono_font_family.clone())
                        .child(label),
                )
                .child(
                    div()
                        .flex_none()
                        .text_size(dp(11.))
                        .text_color(t.text_faint)
                        .child(meta),
                )
                .child(
                    div()
                        .id(("close-result", i))
                        .debug_selector(move || format!("close-result-{i}"))
                        .flex_none()
                        .p(dp(2.))
                        .rounded(dp(4.))
                        .text_color(t.text_faint)
                        .hover(|s| s.bg(t.hover).text_color(t.text))
                        .child(Icon::new(IconName::X).xsmall())
                        .on_click(cx.listener(move |this, _, window, cx| {
                            cx.stop_propagation();
                            this.close_result(i, window, cx);
                        })),
                )
                .on_click(cx.listener(move |this, _, window, cx| {
                    if i == this.active || this.blocked_by_edits(window, cx) {
                        return;
                    }
                    this.active = i;
                    this.show_active(cx);
                    cx.notify();
                }))
                .into_any_element()
        };
        let first_line = |s: &str| {
            s.split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .chars()
                .take(40)
                .collect::<String>()
        };
        for (i, r) in self.results.iter().enumerate() {
            let (dot, meta) = if r.columns.is_empty() {
                (
                    t.warning,
                    format!("{} affected · {} ms", r.affected.unwrap_or(0), r.elapsed_ms),
                )
            } else {
                (
                    t.success,
                    format!("{} · {} ms", rows_label(r.rows.len()), r.elapsed_ms),
                )
            };
            tabs.push(tab(i, dot, first_line(&r.statement), meta, cx));
        }
        if self.error.is_some() && self.plan.is_none() {
            let label = self
                .error_stmt
                .as_deref()
                .map(first_line)
                .unwrap_or_else(|| "Error".into());
            tabs.push(tab(self.results.len(), t.danger, label, "error".into(), cx));
        }
        h_flex()
            .id("result-tabs")
            .h(dp(34.))
            .flex_none()
            .overflow_x_scroll()
            .border_b_1()
            .border_color(t.border)
            .bg(t.sidebar)
            .children(tabs)
            .into_any_element()
    }

    fn render_results(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let t = tokens(cx);
        let theme = cx.theme().clone();
        let showing_error = self.error.is_some() && self.active >= self.results.len();
        let current = self.results.get(self.active).filter(|_| !showing_error);
        let summary = match current {
            _ if self.running => "Running…".to_string(),
            _ if self.plan.is_some() => match self.shared.cfg.driver {
                Driver::Sqlite => "Query plan · SQLite reports the steps, not their cost".into(),
                _ => "Query plan · bars show each step's own share of the cost / time".into(),
            },
            _ if showing_error => "The batch stopped here".to_string(),
            None => "⌘↵ runs the selection, or everything".to_string(),
            Some(r) if r.columns.is_empty() => {
                format!("{} affected · {} ms", r.affected.unwrap_or(0), r.elapsed_ms)
            }
            Some(r) => {
                let limited = self.limit.is_some_and(|n| r.rows.len() == n)
                    && r.statement
                        .trim_end()
                        .ends_with(&format!("LIMIT {}", r.rows.len()));
                format!(
                    "{} · {} ms{}",
                    rows_label(r.rows.len()),
                    r.elapsed_ms,
                    if limited { " · limited" } else { "" }
                )
            }
        };
        let body = if self.running && loading::visible(self.run_started) {
            let cols = self.grid.read(cx).delegate().columns.len().max(4);
            loading::skeleton_rows("query-skeleton", cols, 12, cx)
        } else if let (Some(p), None) = (&self.plan, &self.error) {
            self.render_plan(p, cx)
        } else if showing_error {
            let e = self.error.clone().unwrap_or_default();
            v_flex()
                .id("query-error")
                .size_full()
                .overflow_y_scroll()
                .p(dp(16.))
                .gap(dp(8.))
                .child(
                    h_flex()
                        .gap(dp(8.))
                        .text_size(dp(13.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(t.danger)
                        .child(Icon::new(IconName::CircleX).small())
                        .child("Query failed"),
                )
                .child(
                    div()
                        .p(dp(12.))
                        .rounded(dp(8.))
                        .border_1()
                        .border_color(t.danger.opacity(0.35))
                        .bg(t.danger.opacity(0.08))
                        .font_family(theme.mono_font_family.clone())
                        .text_size(dp(12.))
                        .text_color(t.text)
                        .children(e.lines().map(|l| div().child(l.to_string()))),
                )
                .when(self.results.is_empty() && self.error_stmt.is_some(), |d| {
                    d.child(
                        div()
                            .text_size(dp(12.))
                            .text_color(t.text_muted)
                            .child("The failing statement is underlined in the editor."),
                    )
                })
                .into_any_element()
        } else if current.is_some_and(|r| r.columns.is_empty()) {
            div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .text_color(t.text_muted)
                .child(summary.clone())
                .into_any_element()
        } else if current.is_none() && !self.running {
            v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .gap(dp(8.))
                .text_size(dp(12.5))
                .text_color(t.text_muted)
                .child(Icon::new(IconName::Play).text_color(t.text_faint))
                .child("Run a query to see results here.")
                .child(
                    h_flex()
                        .gap(dp(6.))
                        .child(kbd("⌘↵", cx))
                        .child("run")
                        .child(kbd("⌘E", cx))
                        .child("explain"),
                )
                .into_any_element()
        } else {
            let grid = cell_edit::editable(data_table(&self.grid, cx), cx);
            if self.show_detail {
                let types: Vec<Option<String>> = {
                    let cols = &self.grid.read(cx).delegate().columns;
                    cols.iter()
                        .map(|c| {
                            let t = self.target.as_ref()?;
                            t.columns
                                .iter()
                                .find(|i| &i.name == c)
                                .map(|i| i.data_type.clone())
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
        // One result needs no tabs; two or more (or an error after some) do.
        let shown = self.results.len() + self.error.is_some() as usize;
        let has_tabs = self.plan.is_none() && shown > 1;
        let read_only = self
            .edit_note
            .clone()
            .filter(|_| self.plan.is_none() && current.is_some_and(|r| !r.columns.is_empty()));
        v_flex()
            .size_full()
            .when(has_tabs && !self.running, |d| {
                d.child(self.render_result_tabs(cx))
            })
            .child(
                div().flex_1().min_h_0().relative().child(body).when_some(
                    self.run_started
                        .filter(|s| self.running && loading::visible(Some(*s))),
                    |d, since| {
                        d.child(loading::chip(
                            "Running query",
                            since,
                            Some(
                                Button::new("stop-run")
                                    .cursor_pointer()
                                    .label("Stop ⌘.")
                                    .on_click(cx.listener(|q, _, _, cx| q.cancel(cx))),
                            ),
                            cx,
                        ))
                    },
                ),
            )
            .child(
                h_flex()
                    .h(dp(30.))
                    .flex_none()
                    .px(dp(10.))
                    .border_t_1()
                    .border_color(t.border)
                    .bg(t.sidebar)
                    .gap(dp(8.))
                    .text_size(dp(11.5))
                    .text_color(t.text_muted)
                    .child(summary)
                    .when_some(read_only, |d, note| {
                        let why = note
                            .strip_prefix("Read-only: ")
                            .unwrap_or(&note)
                            .to_string();
                        d.child(
                            h_flex()
                                .gap(dp(6.))
                                .min_w_0()
                                .child(
                                    h_flex()
                                        .gap(dp(4.))
                                        .flex_none()
                                        .h(dp(18.))
                                        .px(dp(6.))
                                        .rounded(dp(5.))
                                        .bg(t.hover)
                                        .text_color(t.text_secondary)
                                        .child(Icon::new(IconName::Lock).xsmall())
                                        .child("Read-only"),
                                )
                                .child(div().truncate().child(why)),
                        )
                    })
                    .when(
                        self.plan.is_none() && self.target.is_some() && current.is_some(),
                        |d| {
                            d.child(
                                div()
                                    .text_color(t.text_faint)
                                    .child("Editable · ↵ or type to edit a cell"),
                            )
                        },
                    )
                    .child(div().flex_1())
                    .map(|bar| {
                        let pending = self.grid.read(cx).delegate().change_count();
                        if pending == 0 {
                            return bar;
                        }
                        bar.child(
                            Button::new("discard-result")
                                .cursor_pointer()
                                .label("Discard")
                                .ghost()
                                .xsmall()
                                .on_click(cx.listener(|this, _, _, cx| this.discard(cx))),
                        )
                        .child(
                            Button::new("preview-result")
                                .cursor_pointer()
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
                                .cursor_pointer()
                                .label(format!("Commit {pending}"))
                                .primary()
                                .xsmall()
                                .tooltip("Commit (⌘S)")
                                .on_click(
                                    cx.listener(|this, _, window, cx| this.commit(window, cx)),
                                ),
                        )
                    })
                    .child(
                        Button::new("toggle-detail")
                            .cursor_pointer()
                            .icon(IconName::PanelRight)
                            .ghost()
                            .xsmall()
                            .selected(self.show_detail)
                            .tooltip("Row detail (⌘⇧I)")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.show_detail = !this.show_detail;
                                cx.notify();
                            })),
                    ),
            )
    }
}

/// An error chain one cause per line, skipping a cause that only repeats
/// the line above it (drivers often wrap their own message).
fn error_text(e: &Error) -> String {
    let mut lines: Vec<String> = vec![];
    for part in e.chain().map(|c| c.to_string()) {
        if lines.last().is_some_and(|l| l.contains(&part)) {
            continue;
        }
        lines.push(part);
    }
    lines.join("\n")
}

/// "1 row", "1,234 rows".
fn rows_label(n: usize) -> String {
    if n == 1 {
        "1 row".into()
    } else {
        format!("{} rows", group(n as u64))
    }
}

/// 12345 → "12,345".
fn group(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, ch) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out
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
        let t = tokens(cx);
        let driver = self.shared.cfg.driver;
        let sql = matches!(driver, Driver::Postgres | Driver::MySql | Driver::Sqlite);
        // A label with its shortcut after it, faint (design.md, Shortcut hint).
        let hinted = |label: &str, keys: &str| {
            h_flex()
                .gap(dp(6.))
                .child(label.to_string())
                .child(div().text_color(t.text_faint).child(keys.to_string()))
        };
        let sep = || div().w(dp(1.)).h(dp(18.)).mx(dp(4.)).bg(t.border);
        let db = self
            .shared
            .conn
            .try_read()
            .map(|c| c.current_database())
            .unwrap_or_default();
        let limit = self.limit;
        let me = cx.entity().downgrade();
        let toolbar = h_flex()
            .h(dp(44.))
            .flex_none()
            .px(dp(10.))
            .gap(dp(4.))
            .border_b_1()
            .border_color(t.border)
            .child(
                Button::new("run")
                    .cursor_pointer()
                    .icon(IconName::Play)
                    .child(hinted("Run", "⌘↵"))
                    .primary()
                    .small()
                    .loading(self.running)
                    .disabled_when(self.running)
                    .tooltip("Run the statement at the cursor, or the selection")
                    .on_click(cx.listener(|this, _, window, cx| this.run(&RunQuery, window, cx))),
            )
            .child(
                Button::new("run-all")
                    .cursor_pointer()
                    .child(hinted("Run all", "⌘⇧↵"))
                    .ghost()
                    .small()
                    .disabled_when(self.running)
                    .on_click(cx.listener(|this, _, window, cx| this.run_all(window, cx))),
            )
            .child(
                Button::new("stop")
                    .cursor_pointer()
                    .icon(IconName::Square)
                    .child(hinted("Stop", "⌘."))
                    .outline()
                    .small()
                    .disabled_when(!self.running)
                    .on_click(cx.listener(|this, _, _, cx| this.cancel(cx))),
            )
            .child(sep())
            .when(sql, |d| {
                d.child(
                    Button::new("explain")
                        .cursor_pointer()
                        .child(hinted("Explain", "⌘E"))
                        .ghost()
                        .small()
                        .disabled_when(self.running)
                        .on_click(
                            cx.listener(|this, _, window, cx| this.explain(false, window, cx)),
                        ),
                )
                .child(
                    Button::new("format")
                        .cursor_pointer()
                        .child(hinted("Format", "⌘⇧F"))
                        .ghost()
                        .small()
                        .on_click(cx.listener(|this, _, window, cx| this.format(window, cx))),
                )
                .child(sep())
            })
            .child(
                Button::new("save-query")
                    .cursor_pointer()
                    .child(hinted("Save", "⌘⇧S"))
                    .ghost()
                    .small()
                    .on_click(cx.listener(|this, _, window, cx| this.save(window, cx))),
            )
            .child(
                Button::new("history")
                    .cursor_pointer()
                    .child(hinted("History", "⌘Y"))
                    .ghost()
                    .small()
                    .on_click(|_, window, cx| window.dispatch_action(Box::new(QueryHistory), cx)),
            )
            .child(div().flex_1())
            .when(!db.is_empty(), |d| {
                d.child(
                    div()
                        .text_size(dp(11.5))
                        .text_color(t.text_muted)
                        .child("Run on"),
                )
                .child(
                    Button::new("run-on")
                        .cursor_pointer()
                        .icon(IconName::Database)
                        .label(db.clone())
                        .outline()
                        .small()
                        .dropdown_caret(true)
                        .tooltip("Switch database (⌘K)")
                        .on_click(|_, window, cx| {
                            window.dispatch_action(Box::new(SwitchDatabase), cx)
                        }),
                )
            })
            .when(sql, |d| {
                d.child(
                    div()
                        .pl(dp(6.))
                        .text_size(dp(11.5))
                        .text_color(t.text_muted)
                        .child("Limit"),
                )
                .child(
                    Button::new("limit")
                        .cursor_pointer()
                        .label(match limit {
                            Some(n) => rows_label(n).replace(" rows", ""),
                            None => "None".into(),
                        })
                        .outline()
                        .small()
                        .dropdown_caret(true)
                        .tooltip("Adds LIMIT to SELECTs that have none")
                        .dropdown_menu(move |menu, _, _| {
                            LIMITS.iter().fold(menu, |menu, &choice| {
                                let me = me.clone();
                                menu.item(
                                    PopupMenuItem::new(match choice {
                                        Some(n) => rows_label(n),
                                        None => "No limit".into(),
                                    })
                                    .checked(choice == limit)
                                    .on_click(
                                        move |_, _, cx| {
                                            me.update(cx, |this, cx| {
                                                this.limit = choice;
                                                cx.notify();
                                            })
                                            .ok();
                                        },
                                    ),
                                )
                            })
                        }),
                )
            });
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
    use anyhow::anyhow;

    #[test]
    fn error_chain_one_cause_per_line() {
        let e = anyhow!("no such column: nope")
            .context("error returned from database: no such column: nope")
            .context("statement 3 of 3 failed.");
        assert_eq!(
            error_text(&e),
            "statement 3 of 3 failed.\nerror returned from database: no such column: nope"
        );
    }

    fn col(name: &str, pk: bool) -> ColumnInfo {
        ColumnInfo {
            name: name.into(),
            data_type: "text".into(),
            nullable: true,
            default: None,
            is_pk: pk,
            enum_values: vec![],
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
