//! SQL / command editor with a results pane. ⌘↵ runs the selection, or the
//! whole editor when nothing is selected. Each statement gets a result tab.

use std::rc::Rc;

use gpui::{
    App, ClipboardItem, Context, Entity, FocusHandle, Focusable, IntoElement, ParentElement,
    Render, SharedString, Styled, Subscription, Window, div, prelude::*, px, rems,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::{Editor, EditorState};
use gpui_component::menu::PopupMenuItem;
use gpui_component::notification::Notification;
use gpui_component::tab::{Tab, TabBar};
use gpui_component::table::{TableEvent, TableState};
use gpui_component::{
    ActiveTheme as _, Sizable as _, WindowExt as _, h_flex, resizable_panel, v_flex, v_resizable,
};
use gpui_kit_assets::IconName;

use super::cell_edit::{self, EditStart};
use super::grid::{Grid, data_table};
use super::{RunQuery, Shared};
use crate::db::{ColumnInfo, DbObject, Driver, EnvTag, ObjectKind, QueryResult};
use crate::runtime;

pub struct QueryTab {
    shared: Rc<Shared>,
    pub title: String,
    editor: Entity<EditorState>,
    results: Vec<QueryResult>,
    active: usize,
    grid: Entity<TableState<Grid>>,
    running: bool,
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
            let w2 = weak.clone();
            menu.item(
                PopupMenuItem::new("Copy Row as JSON").on_click(move |_, window, cx| {
                    w.update(cx, |t, cx| {
                        let json = t.grid.read(cx).delegate().row_json(row);
                        cx.write_to_clipboard(ClipboardItem::new_string(json));
                        window.push_notification("Row copied as JSON", cx);
                    })
                    .ok();
                }),
            )
            .item(
                PopupMenuItem::new("Copy Result as CSV").on_click(move |_, window, cx| {
                    w2.update(cx, |t, cx| {
                        let csv = t.grid.read(cx).delegate().to_csv();
                        cx.write_to_clipboard(ClipboardItem::new_string(csv));
                        window.push_notification("Result copied as CSV", cx);
                    })
                    .ok();
                }),
            )
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
                if let TableEvent::DoubleClickedCell(r, c) = ev {
                    cell_edit::start_edit(this, *r, *c, EditStart::Keep, window, cx);
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
        if self.shared.cfg.tag == EnvTag::Production
            && !crate::db::safety::is_read_only(self.shared.cfg.driver, &text)
        {
            let weak = cx.entity().downgrade();
            let shown: SharedString = text.chars().take(600).collect::<String>().into();
            window.open_alert_dialog(cx, move |d, _, _| {
                let weak = weak.clone();
                let text = text.clone();
                d.title("Run on PRODUCTION?")
                    .description(shown.clone())
                    .on_ok(move |_, window, cx| {
                        weak.update(cx, |t, cx| t.execute(text.clone(), window, cx))
                            .ok();
                        true
                    })
            });
        } else {
            self.execute(text, window, cx);
        }
    }

    fn execute(&mut self, text: String, window: &mut Window, cx: &mut Context<Self>) {
        self.last_run = text.clone();
        self.running = true;
        self.error = None;
        cx.notify();
        let conn = self.shared.conn.clone();
        let start = std::time::Instant::now();
        let task = runtime::spawn(async move { conn.read().await.execute(&text).await });
        cx.spawn_in(window, async move |this, cx| {
            let res = task.await;
            this.update_in(cx, |this, window, cx| {
                this.running = false;
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
                        window.push_notification(Notification::error("Query failed"), cx);
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
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
                let cols = st.map(|s| s.columns).unwrap_or_default();
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
            .unwrap_or_default()
    }

    pub fn commit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        cell_edit::finish_edit(self, window, cx);
        if self.target.is_none() || !self.is_dirty(cx) {
            return;
        }
        if self.shared.cfg.tag == EnvTag::Production {
            let preview: SharedString = self.preview(cx).into();
            let weak = cx.entity().downgrade();
            window.open_alert_dialog(cx, move |d, _, _| {
                let weak = weak.clone();
                d.title("Commit to PRODUCTION?")
                    .description(preview.clone())
                    .on_ok(move |_, window, cx| {
                        weak.update(cx, |t, cx| t.do_commit(window, cx)).ok();
                        true
                    })
            });
        } else {
            self.do_commit(window, cx);
        }
    }

    fn do_commit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(t) = self.target.clone() else { return };
        let changes = self.grid.read(cx).delegate().changes();
        let n = changes.len();
        let conn = self.shared.conn.clone();
        let task =
            runtime::spawn(
                async move { conn.read().await.commit(&t.obj, &t.columns, &changes).await },
            );
        cx.spawn_in(window, async move |this, cx| {
            let res = task.await;
            this.update_in(cx, |this, window, cx| match res {
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
        let body = if let Some(e) = &self.error {
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
            cell_edit::editable(data_table(&self.grid, cx), cx).into_any_element()
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
            .child(
                Button::new("run")
                    .icon(IconName::Play)
                    .label("Run")
                    .primary()
                    .small()
                    .loading(self.running)
                    .tooltip("Run selection or all (⌘↵)")
                    .on_click(cx.listener(|this, _, window, cx| this.run(&RunQuery, window, cx))),
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
