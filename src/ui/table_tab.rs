//! A table / collection / key-space tab: Data view (filter bar, editable grid,
//! pager) and Structure view (columns + indexes).

use std::rc::Rc;

use gpui::{
    App, ClipboardItem, Context, Entity, FocusHandle, Focusable, IntoElement, ParentElement,
    Render, SharedString, Styled, Subscription, Window, div, prelude::*, px, rems,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::menu::PopupMenuItem;
use gpui_component::notification::Notification;
use gpui_component::select::{Select, SelectState};
use gpui_component::tab::{Tab, TabBar};
use gpui_component::table::{TableEvent, TableState};
use gpui_component::{
    ActiveTheme as _, Disableable as _, IndexPath, Selectable as _, Sizable as _, WindowExt as _,
    h_flex, v_flex,
};
use gpui_kit_assets::IconName;

use super::cell_edit::{self, EditStart};
use super::copy;
use super::row_panel;
use super::grid::{Grid, data_table};
use super::{
    AddRow, Commit, CopyCell, DeleteRows, Discard, SearchList, SetNull, Shared, ToggleFilter,
    search_list,
};
use crate::db::{DbObject, EnvTag, Filter, FilterOp, QueryResult, Structure, TableQuery};
use crate::runtime;

pub enum TableTabEvent {
    /// Open `table` showing only rows matching `filters`.
    OpenReference {
        schema: Option<String>,
        table: String,
        filters: Vec<Filter>,
    },
    /// Open SQL in a new query tab.
    OpenQuery(String),
}

impl gpui::EventEmitter<TableTabEvent> for TableTab {}

const PAGE_SIZES: [usize; 4] = [100, 300, 1000, 5000];

struct FilterRow {
    column: Entity<SelectState<SearchList>>,
    op: Entity<SelectState<Vec<&'static str>>>,
    value: Entity<InputState>,
}

pub struct TableTab {
    shared: Rc<Shared>,
    pub object: DbObject,
    table: Entity<TableState<Grid>>,
    structure: Option<Structure>,
    struct_cols: Entity<TableState<Grid>>,
    struct_idx: Entity<TableState<Grid>>,
    struct_fks: Entity<TableState<Grid>>,
    show_structure: bool,
    /// Row detail panel beside the data grid.
    pub show_detail: bool,
    /// Structure view section: columns, indexes, foreign keys, SQL.
    struct_section: usize,
    /// A commit is on its way; ⌘S again mustn't send it twice.
    committing: bool,
    /// Filters this tab was opened with (following a foreign key); shown as
    /// a removable chip, applied on top of the filter bar.
    fixed: Vec<Filter>,
    filters: Vec<FilterRow>,
    show_filters: bool,
    page: usize,
    page_size: usize,
    total: Option<u64>,
    elapsed: u128,
    error: Option<String>,
    focus: FocusHandle,
    edit_sub: Option<Subscription>,
    /// Bumped per reload; only the latest response is applied, so a slow
    /// earlier page can't overwrite a newer one.
    load_seq: u64,
    _subs: Vec<Subscription>,
}

impl TableTab {
    pub fn new(
        shared: Rc<Shared>,
        object: DbObject,
        fixed: Vec<Filter>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let weak = cx.entity().downgrade();

        let mut grid = Grid::default();
        let w = weak.clone();
        grid.on_sort = Some(Rc::new(move |_, _, window, cx| {
            // The grid is mid-update; reload once it's done.
            let w = w.clone();
            window.defer(cx, move |_, cx| {
                w.update(cx, |t, cx| {
                    t.page = 0;
                    t.reload(cx);
                })
                .ok();
            });
        }));
        let w = weak.clone();
        grid.on_menu = Some(Rc::new(move |row, menu, _, _| {
            let item =
                |label: &'static str,
                 f: fn(&mut TableTab, usize, &mut Window, &mut Context<TableTab>)| {
                    let w = w.clone();
                    PopupMenuItem::new(label).on_click(move |_, window, cx| {
                        w.update(cx, |t, cx| f(t, row, window, cx)).ok();
                    })
                };
            menu.item(item("Edit Cell", |t, row, window, cx| {
                let col = t
                    .table
                    .read(cx)
                    .selected_cell()
                    .map(|(_, c)| c)
                    .unwrap_or(0);
                cell_edit::start_edit(t, row, col, EditStart::Keep, window, cx);
            }))
            .item(item("Copy Cell", |t, _, window, cx| {
                t.copy_cell(window, cx)
            }))
            .separator()
            .map(|menu| copy::copy_items(menu, w.clone(), row))
            .item(item("Import CSV…", |t, _, window, cx| t.import_csv(window, cx)))
            .separator()
            .item(item("Set NULL", |t, _, window, cx| t.set_null(window, cx)))
            .item(item("Duplicate Row", |t, row, _, cx| {
                t.duplicate_row(row, cx)
            }))
            .item(item("Delete Row", |t, row, _, cx| {
                t.table.update(cx, |tb, cx| {
                    tb.delegate_mut().toggle_delete(row);
                    cx.notify();
                });
                cx.notify();
            }))
        }));

        let w = weak.clone();
        grid.on_follow = Some(Rc::new(move |row, col, _, cx| {
            w.update(cx, |t, cx| t.follow(row, col, cx)).ok();
        }));

        let table = cx.new(|cx| {
            TableState::new(grid, window, cx)
                .row_selectable(true)
                .cell_selectable(true)
                .col_resizable(true)
                .sortable(true)
        });
        let read_only = |window: &mut Window, cx: &mut Context<Self>| {
            cx.new(|cx| {
                TableState::new(Grid::default(), window, cx)
                    .cell_selectable(true)
                    .col_resizable(true)
            })
        };
        let struct_cols = read_only(window, cx);
        let struct_idx = read_only(window, cx);
        let struct_fks = read_only(window, cx);

        let subs = vec![
            cx.subscribe_in(&table, window, |this, _, ev: &TableEvent, window, cx| {
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

        let mut this = Self {
            shared,
            object,
            table,
            structure: None,
            struct_cols,
            struct_idx,
            struct_fks,
            show_structure: false,
            show_detail: false,
            committing: false,
            struct_section: 0,
            fixed,
            filters: Vec::new(),
            show_filters: false,
            page: 0,
            page_size: PAGE_SIZES[1],
            total: None,
            elapsed: 0,
            error: None,
            focus: cx.focus_handle(),
            edit_sub: None,
            load_seq: 0,
            _subs: subs,
        };
        this.reload(cx);
        this
    }

    #[cfg(test)]
    pub fn loaded_rows(&self, cx: &App) -> usize {
        self.table.read(cx).delegate().base_rows
    }

    /// Selects a cell and focuses the grid, as a click would.
    #[cfg(test)]
    pub fn select_cell(&mut self, r: usize, c: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.table.update(cx, |t, cx| t.set_selected_cell(r, c, cx));
        let fh = self.table.focus_handle(cx);
        window.focus(&fh, cx);
    }

    #[cfg(test)]
    pub fn editing_cell(&self, cx: &App) -> Option<(usize, usize)> {
        self.table
            .read(cx)
            .delegate()
            .editing
            .as_ref()
            .map(|(r, c, _)| (*r, *c))
    }

    /// The staged (uncommitted) value of a cell, if it was edited.
    #[cfg(test)]
    pub fn staged(&self, r: usize, c: usize, cx: &App) -> Option<Option<String>> {
        self.table.read(cx).delegate().edits.get(&(r, c)).cloned()
    }

    #[cfg(test)]
    pub fn cell(&self, r: usize, c: usize, cx: &App) -> Option<String> {
        self.table.read(cx).delegate().cell_string(r, c)
    }

    #[cfg(test)]
    pub fn column_names(&self) -> Vec<String> {
        self.structure
            .iter()
            .flat_map(|s| s.columns.iter().map(|c| c.name.clone()))
            .collect()
    }

    #[cfg(test)]
    pub fn has_structure(&self) -> bool {
        self.structure.is_some()
    }

    #[cfg(test)]
    pub fn links(&self, cx: &App) -> Vec<bool> {
        self.table.read(cx).delegate().links.clone()
    }

    #[cfg(test)]
    pub fn page(&self) -> usize {
        self.page
    }

    #[cfg(test)]
    pub fn stage_cell(&mut self, r: usize, c: usize, v: &str, cx: &mut Context<Self>) {
        self.table
            .update(cx, |t, _| t.delegate_mut().set_cell(r, c, Some(v.into())));
    }

    #[cfg(test)]
    pub fn next_page_for_test(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.reload_unless_dirty(window, cx, |t| t.page += 1);
    }

    pub fn title(&self) -> String {
        self.object.title()
    }

    pub fn is_dirty(&self, cx: &App) -> bool {
        self.table.read(cx).delegate().is_dirty()
    }

    fn filter_values(&self, cx: &App) -> Vec<Filter> {
        self.filters
            .iter()
            .filter_map(|f| {
                let column = f.column.read(cx).selected_value()?.clone();
                let op_label = *f.op.read(cx).selected_value()?;
                let op = FilterOp::ALL.into_iter().find(|o| o.label() == op_label)?;
                let value = f.value.read(cx).value().to_string();
                if op.needs_value() && value.is_empty() {
                    return None;
                }
                Some(Filter { column, op, value })
            })
            .collect()
    }

    /// For user-triggered reloads (paging, filters…): a reload replaces the
    /// rows staged edits refer to, so refuse while there are any.
    fn reload_unless_dirty(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        change: impl FnOnce(&mut Self),
    ) {
        if self.is_dirty(cx) {
            window.push_notification(
                Notification::warning("Commit (⌘S) or discard (⌘⇧⌫) your changes first"),
                cx,
            );
            return;
        }
        change(self);
        self.reload(cx);
    }

    /// The current page, with the tab's filters and sort.
    fn current_query(&self, cx: &App) -> TableQuery {
        let grid = self.table.read(cx).delegate();
        let order = grid
            .sort
            .and_then(|(c, asc)| Some((grid.columns.get(c)?.clone(), asc)));
        TableQuery {
            object: self.object.clone(),
            filters: self
                .fixed
                .iter()
                .cloned()
                .chain(if self.show_filters {
                    self.filter_values(cx)
                } else {
                    vec![]
                })
                .collect(),
            order,
            limit: self.page_size,
            offset: self.page * self.page_size,
        }
    }

    pub fn reload(&mut self, cx: &mut Context<Self>) {
        let q = self.current_query(cx);
        self.table.update(cx, |t, cx| {
            t.delegate_mut().loading = true;
            cx.notify();
        });
        let conn = self.shared.conn.clone();
        let obj = self.object.clone();
        let need_structure = self.structure.is_none();
        self.load_seq += 1;
        let seq = self.load_seq;
        let task = runtime::spawn(async move {
            let c = conn.read().await;
            let st = if need_structure {
                Some(c.structure(&obj).await?)
            } else {
                None
            };
            let data = c.fetch_table(&q).await?;
            Ok((st, data))
        });
        cx.spawn(async move |this, cx| {
            let res = task.await;
            this.update(cx, |this, cx| {
                if seq == this.load_seq {
                    this.apply_load(res, cx);
                }
            })
            .ok();
        })
        .detach();
    }

    fn apply_load(
        &mut self,
        res: anyhow::Result<(Option<Structure>, (QueryResult, Option<u64>))>,
        cx: &mut Context<Self>,
    ) {
        self.table.update(cx, |t, cx| {
            t.delegate_mut().loading = false;
            cx.notify();
        });
        let (st, (data, total)) = match res {
            Ok(v) => v,
            Err(e) => {
                self.error = Some(format!("{e:#}"));
                // Rows from before would pass for current (and be editable).
                // A reload only starts without staged edits, so none are lost.
                self.table.update(cx, |t, cx| {
                    t.delegate_mut().set_result(&QueryResult::default());
                    t.refresh(cx);
                });
                cx.notify();
                return;
            }
        };
        self.error = None;
        if let Some(st) = st {
            let cols = QueryResult {
                columns: ["column", "type", "nullable", "default", "primary key"]
                    .map(String::from)
                    .to_vec(),
                rows: st
                    .columns
                    .iter()
                    .map(|c| {
                        vec![
                            Some(c.name.clone()),
                            Some(c.data_type.clone()),
                            Some(if c.nullable { "YES" } else { "NO" }.into()),
                            c.default.clone(),
                            Some(if c.is_pk { "YES" } else { "" }.into()),
                        ]
                    })
                    .collect(),
                ..Default::default()
            };
            self.struct_cols.update(cx, |t, cx| {
                t.delegate_mut().set_result(&cols);
                t.refresh(cx);
            });
            let idx = st.indexes.clone();
            self.struct_idx.update(cx, |t, cx| {
                t.delegate_mut().set_result(&idx);
                t.refresh(cx);
            });
            let fks = QueryResult {
                columns: ["name", "columns", "references", "on update", "on delete"]
                    .map(String::from)
                    .to_vec(),
                rows: st
                    .foreign_keys
                    .iter()
                    .map(|f| {
                        let table = match &f.ref_schema {
                            Some(s) if !s.is_empty() => format!("{s}.{}", f.ref_table),
                            _ => f.ref_table.clone(),
                        };
                        vec![
                            Some(f.name.clone()),
                            Some(f.columns.join(", ")),
                            Some(format!("{table} ({})", f.ref_columns.join(", "))),
                            Some(f.on_update.clone()),
                            Some(f.on_delete.clone()),
                        ]
                    })
                    .collect(),
                ..Default::default()
            };
            self.struct_fks.update(cx, |t, cx| {
                t.delegate_mut().set_result(&fks);
                t.refresh(cx);
            });
            self.structure = Some(st);
        }
        let pk_names: Vec<String> = self
            .structure
            .as_ref()
            .map(|s| {
                s.columns
                    .iter()
                    .filter(|c| c.is_pk)
                    .map(|c| c.name.clone())
                    .collect()
            })
            .unwrap_or_default();
        self.total = total;
        self.elapsed = data.elapsed_ms;
        let fk_cols: Vec<String> = self
            .structure
            .iter()
            .flat_map(|s| &s.foreign_keys)
            .flat_map(|f| f.columns.clone())
            .collect();
        self.table.update(cx, |t, cx| {
            let g = t.delegate_mut();
            g.set_result(&data);
            g.pk = g.columns.iter().map(|c| pk_names.contains(c)).collect();
            g.links = g.columns.iter().map(|c| fk_cols.contains(c)).collect();
            g.editable = vec![true; g.columns.len()];
            t.refresh(cx);
            cx.notify();
        });
        cx.notify();
    }

    /// After the columns changed: fetch the structure again with the rows.
    pub fn reload_structure(&mut self, cx: &mut Context<Self>) {
        self.structure = None;
        self.reload(cx);
    }

    fn edit_columns(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(st) = &self.structure else { return };
        let me = cx.entity().downgrade();
        let on_done: super::table_editor::OnDone = Rc::new(move |_, cx| {
            me.update(cx, |t, cx| t.reload_structure(cx)).ok();
        });
        super::table_editor::TableEditor::open(
            self.shared.clone(),
            Some(self.object.clone()),
            st.columns.clone(),
            None,
            on_done,
            window,
            cx,
        );
    }

    #[cfg(test)]
    pub fn edit_columns_for_test(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Entity<super::table_editor::TableEditor>> {
        let st = self.structure.as_ref()?;
        let me = cx.entity().downgrade();
        let on_done: super::table_editor::OnDone = Rc::new(move |_, cx| {
            me.update(cx, |t, cx| t.reload_structure(cx)).ok();
        });
        Some(super::table_editor::TableEditor::open(
            self.shared.clone(),
            Some(self.object.clone()),
            st.columns.clone(),
            None,
            on_done,
            window,
            cx,
        ))
    }

    /// → on a foreign-key cell: open the referenced table filtered to the
    /// row(s) this one points at.
    fn follow(&mut self, row: usize, col: usize, cx: &mut Context<Self>) {
        let g = self.table.read(cx).delegate();
        let Some(column) = g.columns.get(col).cloned() else {
            return;
        };
        let Some(st) = &self.structure else { return };
        // Prefer a single-column key on this column over a composite one.
        let Some(fk) = st
            .foreign_keys
            .iter()
            .filter(|f| f.columns.contains(&column))
            .min_by_key(|f| f.columns.len())
        else {
            return;
        };
        let mut filters = vec![];
        for (c, rc) in fk.columns.iter().zip(&fk.ref_columns) {
            let Some(ix) = g.columns.iter().position(|x| x == c) else {
                return;
            };
            // A NULL part of the key references nothing.
            let Some(value) = g.cell_string(row, ix) else {
                return;
            };
            filters.push(Filter {
                column: rc.clone(),
                op: FilterOp::Eq,
                value,
            });
        }
        cx.emit(TableTabEvent::OpenReference {
            schema: fk.ref_schema.clone().filter(|s| !s.is_empty()),
            table: fk.ref_table.clone(),
            filters,
        });
    }

    fn selected_cell(&self, cx: &App) -> Option<(usize, usize)> {
        self.table.read(cx).selected_cell()
    }

    fn selected_row(&self, cx: &App) -> Option<usize> {
        let t = self.table.read(cx);
        t.selected_row().or(t.selected_cell().map(|(r, _)| r))
    }

    fn copy_cell(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let table = if self.show_structure {
            &self.struct_cols
        } else {
            &self.table
        };
        let t = table.read(cx);
        let text = match (t.selected_cell(), t.selected_row()) {
            (Some((r, c)), _) => t.delegate().cell_string(r, c).unwrap_or_default(),
            (None, Some(r)) => copy::row_tsv(t.delegate(), r),
            _ => return,
        };
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        window.push_notification("Copied", cx);
    }

    fn set_null(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        if let Some((r, c)) = self.selected_cell(cx) {
            self.table.update(cx, |t, cx| {
                t.delegate_mut().set_cell(r, c, None);
                cx.notify();
            });
        }
    }

    fn duplicate_row(&mut self, row: usize, cx: &mut Context<Self>) {
        self.table.update(cx, |t, cx| {
            let g = t.delegate_mut();
            let values: Vec<_> = (0..g.columns.len())
                .map(|c| {
                    if g.pk.get(c).copied().unwrap_or(false) {
                        None
                    } else {
                        g.cell_string(row, c)
                    }
                })
                .collect();
            let r = g.add_row();
            for (c, v) in values.into_iter().enumerate() {
                if v.is_some() {
                    g.set_cell(r, c, v);
                }
            }
            t.scroll_to_row(r, cx);
            cx.notify();
        });
    }

    pub fn add_row(&mut self, _: &AddRow, window: &mut Window, cx: &mut Context<Self>) {
        if self.show_structure {
            return;
        }
        let r = self.table.update(cx, |t, cx| {
            let r = t.delegate_mut().add_row();
            t.scroll_to_row(r, cx);
            t.set_selected_cell(r, 0, cx);
            r
        });
        cell_edit::start_edit(self, r, 0, EditStart::Keep, window, cx);
    }

    pub fn delete_rows(&mut self, _: &DeleteRows, _window: &mut Window, cx: &mut Context<Self>) {
        if let Some(r) = self.selected_row(cx) {
            self.table.update(cx, |t, cx| {
                t.delegate_mut().toggle_delete(r);
                cx.notify();
            });
            cx.notify();
        }
    }

    pub fn discard(&mut self, _: &Discard, _window: &mut Window, cx: &mut Context<Self>) {
        self.table.update(cx, |t, cx| {
            t.delegate_mut().discard();
            cx.notify();
        });
        cx.notify();
    }

    pub fn refresh(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.is_dirty(cx) {
            window.push_notification(
                Notification::warning("Commit (⌘S) or discard (⌘⇧⌫) your changes first"),
                cx,
            );
            return;
        }
        self.structure = None;
        self.reload(cx);
    }

    pub fn toggle_filter(&mut self, _: &ToggleFilter, window: &mut Window, cx: &mut Context<Self>) {
        if self.show_filters {
            // Hiding clears the filters, which reloads the rows.
            self.reload_unless_dirty(window, cx, |t| {
                t.show_filters = false;
                t.page = 0;
            });
        } else {
            self.show_filters = true;
            if self.filters.is_empty() {
                self.add_filter(window, cx);
            }
            if let Some(f) = self.filters.last() {
                f.value.update(cx, |i, cx| i.focus(window, cx));
            }
        }
        cx.notify();
    }

    fn add_filter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let columns: Vec<String> = self.table.read(cx).delegate().columns.clone();
        let column = cx.new(|cx| {
            SelectState::new(search_list(columns), Some(IndexPath::new(0)), window, cx)
                .searchable(true)
        });
        let ops: Vec<&'static str> = FilterOp::ALL.iter().map(|o| o.label()).collect();
        let op = cx.new(|cx| SelectState::new(ops, Some(IndexPath::new(0)), window, cx));
        let value = cx.new(|cx| InputState::new(window, cx).placeholder("Value — Enter to apply"));
        self._subs.push(
            cx.subscribe_in(&value, window, |this, _, ev: &InputEvent, window, cx| {
                if let InputEvent::PressEnter { .. } = ev {
                    this.reload_unless_dirty(window, cx, |t| t.page = 0);
                }
            }),
        );
        self.filters.push(FilterRow { column, op, value });
    }

    /// Import CSV…: pick a file, confirm the column mapping, insert every
    /// row in one transaction.
    pub fn import_csv(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.is_dirty(cx) {
            window.push_notification(
                Notification::warning("Commit (⌘S) or discard (⌘⇧⌫) your changes first"),
                cx,
            );
            return;
        }
        let rx = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Import CSV".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = rx.await else {
                return;
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };
            this.update_in(cx, |t, window, cx| t.import_csv_file(&path, window, cx))
                .ok();
        })
        .detach();
    }

    pub fn import_csv_file(
        &mut self,
        path: &std::path::Path,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let fail = |msg: String, window: &mut Window, cx: &mut Context<Self>| {
            window.push_notification(Notification::error(msg).title("Import failed"), cx);
        };
        if self.shared.cfg.driver == crate::db::Driver::Redis {
            return fail("CSV import isn't available for Redis".into(), window, cx);
        }
        if let crate::db::safety::Gate::Block(why) = crate::db::safety::gate_write(&self.shared.cfg) {
            return fail(why, window, cx);
        }
        let atomic = self
            .shared
            .conn
            .try_read()
            .map(|c| c.commits_atomically())
            .unwrap_or(true);
        let csv = match std::fs::read(path)
            .map_err(anyhow::Error::from)
            .and_then(|b| crate::csv_import::parse(&b))
        {
            Ok(c) => c,
            Err(e) => return fail(format!("{e:#}"), window, cx),
        };
        let Some(columns) = self.structure.as_ref().map(|s| s.columns.clone()) else {
            return fail("The table's columns haven't loaded yet".into(), window, cx);
        };
        let mapping = crate::csv_import::map_columns(&csv.headers, &columns);
        let matched: Vec<String> = csv
            .headers
            .iter()
            .zip(&mapping)
            .filter_map(|(h, m)| m.as_ref().map(|m| if h == m { m.clone() } else { format!("{m} ← {h}") }))
            .collect();
        if matched.is_empty() {
            return fail(
                format!(
                    "No CSV column matches a column of {}. The header row must use the column names.",
                    self.object.title()
                ),
                window,
                cx,
            );
        }
        let ignored: Vec<&String> = csv
            .headers
            .iter()
            .zip(&mapping)
            .filter(|(_, m)| m.is_none())
            .map(|(h, _)| h)
            .collect();
        let n = csv.rows.len();
        let changes = crate::csv_import::changes(&csv, &mapping);
        let mut text = format!(
            "Insert {n} row(s) into {}.\n\nColumns: {}",
            self.object.title(),
            matched.join(", ")
        );
        if !ignored.is_empty() {
            text.push_str(&format!(
                "\nIgnored (no such column): {}",
                ignored.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ")
            ));
        }
        text.push_str("\n\nEmpty cells get the column's default.");
        text.push_str(if atomic {
            " All rows are inserted in one transaction: if one fails, none are."
        } else {
            " If a row fails, the rows before it stay inserted."
        });
        let title = if self.shared.cfg.tag == EnvTag::Production {
            "Import into PRODUCTION?"
        } else {
            "Import CSV?"
        };
        let weak = cx.entity().downgrade();
        let changes = std::rc::Rc::new(changes);
        window.open_alert_dialog(cx, move |d, _, _| {
            let (weak, changes, columns) = (weak.clone(), changes.clone(), columns.clone());
            d.title(title)
                .description(SharedString::from(text.clone()))
                .show_cancel(true)
                .on_ok(move |_, window, cx| {
                    let (changes, columns) = ((*changes).clone(), columns.clone());
                    weak.update(cx, |t, cx| t.run_import(changes, columns, atomic, window, cx))
                        .ok();
                    true
                })
        });
    }

    fn run_import(
        &mut self,
        changes: Vec<crate::db::Change>,
        columns: Vec<crate::db::ColumnInfo>,
        atomic: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let n = changes.len();
        let conn = self.shared.conn.clone();
        let obj = self.object.clone();
        let task = runtime::spawn(async move {
            conn.read().await.commit(&obj, &columns, &changes).await
        });
        cx.spawn_in(window, async move |this, cx| {
            let res = task.await;
            this.update_in(cx, |t, window, cx| {
                match res {
                    Ok(()) => window.push_notification(
                        Notification::success(format!("Imported {n} row(s)")),
                        cx,
                    ),
                    Err(e) => window.push_notification(
                        Notification::error(format!("{e:#}")).title(if atomic {
                            "Import failed: nothing was inserted"
                        } else {
                            "Import stopped: earlier rows were inserted"
                        }),
                        cx,
                    ),
                }
                t.reload(cx);
            })
            .ok();
        })
        .detach();
    }

    pub fn commit(&mut self, _: &Commit, window: &mut Window, cx: &mut Context<Self>) {
        cell_edit::finish_edit(self, window, cx);
        let changes = self.table.read(cx).delegate().changes();
        if changes.is_empty() {
            return;
        }
        let columns = self
            .structure
            .as_ref()
            .map(|s| s.columns.clone())
            .unwrap_or_default();
        let gate = crate::db::safety::gate_write(&self.shared.cfg);
        let preview = if gate == crate::db::safety::Gate::Confirm {
            self.shared
                .conn
                .try_read()
                .map(|c| c.preview(&self.object, &columns, &changes))
                .unwrap_or_else(|_| {
                    "(The connection is busy, so the statements can't be listed right now.)".into()
                })
        } else {
            String::new()
        };
        let weak = cx.entity().downgrade();
        if super::guarded(gate, &self.shared.cfg, "Commit", preview, window, cx, move |w, cx| {
            weak.update(cx, |t, cx| t.do_commit(w, cx)).ok();
        }) {
            self.do_commit(window, cx);
        }
    }

    fn do_commit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.committing {
            return;
        }
        let changes = self.table.read(cx).delegate().changes();
        // Writes need the column types (and keys) from the structure.
        let Some(columns) = self.structure.as_ref().map(|s| s.columns.clone()) else {
            window.push_notification(
                Notification::warning("The table's structure isn't loaded; refresh (⌘R) and try again"),
                cx,
            );
            return;
        };
        self.committing = true;
        cx.notify();
        let conn = self.shared.conn.clone();
        let obj = self.object.clone();
        let n = changes.len();
        let task =
            runtime::spawn(async move { conn.read().await.commit(&obj, &columns, &changes).await });
        cx.spawn_in(window, async move |this, cx| {
            let res = task.await;
            this.update_in(cx, |this, window, cx| {
                this.committing = false;
                cx.notify();
                match res {
                Ok(()) => {
                    window.push_notification(
                        Notification::success(format!("Committed {n} change(s)")),
                        cx,
                    );
                    this.reload(cx);
                }
                Err(e) => {
                    let atomic = this
                        .shared
                        .conn
                        .try_read()
                        .map_or(true, |c| c.commits_atomically());
                    let mut msg = format!("{e:#}");
                    if !atomic {
                        // Some changes may be saved; keeping them all staged
                        // would apply those twice on retry.
                        msg.push_str(". The table was reloaded to show what was saved.");
                        this.table.update(cx, |t, cx| {
                            t.delegate_mut().discard();
                            cx.notify();
                        });
                        this.reload(cx);
                    }
                    window.push_notification(Notification::error(msg).title("Commit failed"), cx)
                }
                }
            })
            .ok();
        })
        .detach();
    }

    fn preview_sql(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let changes = self.table.read(cx).delegate().changes();
        let columns = self
            .structure
            .as_ref()
            .map(|s| s.columns.clone())
            .unwrap_or_default();
        let sql = self
            .shared
            .conn
            .try_read()
            .map(|c| c.preview(&self.object, &columns, &changes))
            .unwrap_or_default();
        window.open_alert_dialog(cx, move |d, _, cx| {
            d.title("Pending changes").description(
                div()
                    .max_h(px(400.))
                    .overflow_hidden()
                    .p_2()
                    .rounded_md()
                    .bg(cx.theme().muted)
                    .font_family(cx.theme().mono_font_family.clone())
                    .text_sm()
                    .child(sql.clone()),
            )
        });
    }

    fn render_filters(&self, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .gap_1()
            .p_2()
            .border_b_1()
            .border_color(cx.theme().border)
            .children(self.filters.iter().enumerate().map(|(i, f)| {
                h_flex()
                    .gap_1()
                    .child(div().w(px(200.)).child(Select::new(&f.column).small()))
                    .child(div().w(px(130.)).child(Select::new(&f.op).small()))
                    .child(
                        div()
                            .flex_1()
                            .child(Input::new(&f.value).small().cleanable(true)),
                    )
                    .child(
                        Button::new(("rm-filter", i))
                            .icon(IconName::Minus)
                            .ghost()
                            .small()
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.reload_unless_dirty(window, cx, |t| {
                                    t.filters.remove(i);
                                    t.page = 0;
                                });
                            })),
                    )
                    .when(i == self.filters.len() - 1, |row| {
                        row.child(
                            Button::new("add-filter")
                                .icon(IconName::Plus)
                                .ghost()
                                .small()
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.add_filter(window, cx);
                                    cx.notify();
                                })),
                        )
                        .child(
                            Button::new("apply-filter")
                                .label("Apply")
                                .primary()
                                .small()
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.reload_unless_dirty(window, cx, |t| t.page = 0);
                                })),
                        )
                    })
            }))
    }

    fn render_footer(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let grid = self.table.read(cx).delegate();
        let pending = grid.change_count();
        let shown = grid.base_rows;
        let pages = self
            .total
            .map(|t| (t as usize).div_ceil(self.page_size).max(1));
        let info = match self.total {
            Some(t) => format!("{shown} of {t} rows · {} ms", self.elapsed),
            None => format!("{shown} rows · {} ms", self.elapsed),
        };
        let page = self.page;
        h_flex()
            .h(rems(2.25))
            .px_2()
            .gap_1()
            .border_t_1()
            .border_color(theme.border)
            .bg(theme.title_bar)
            .child(
                Button::new("view-data")
                    .label("Data")
                    .small()
                    .ghost()
                    .selected(!self.show_structure)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.show_structure = false;
                        cx.notify();
                    })),
            )
            .child(
                Button::new("view-structure")
                    .label("Structure")
                    .small()
                    .ghost()
                    .selected(self.show_structure)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.show_structure = true;
                        cx.notify();
                    })),
            )
            .child(div().w(px(8.)))
            .when(!self.show_structure, |f| f.child(Button::new("toggle-detail")
                    .icon(IconName::PanelRight)
                    .ghost()
                    .small()
                    .selected(self.show_detail)
                    .tooltip("Row detail (⌘⇧I)")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.show_detail = !this.show_detail;
                        cx.notify();
                    }))))
            .when(!self.show_structure, |f| {
                f.child(
                    Button::new("add-row")
                        .icon(IconName::Plus)
                        .ghost()
                        .small()
                        .tooltip("Add row (⌘I)")
                        .on_click(
                            cx.listener(|this, _, window, cx| this.add_row(&AddRow, window, cx)),
                        ),
                )
                .child(
                    Button::new("del-row")
                        .icon(IconName::Minus)
                        .ghost()
                        .small()
                        .tooltip("Delete row (⌘⌫)")
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.delete_rows(&DeleteRows, window, cx)
                        })),
                )
                .child(
                    Button::new("filter")
                        .icon(IconName::ListFilter)
                        .ghost()
                        .small()
                        .selected(self.show_filters)
                        .tooltip("Filter (⌘F)")
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.toggle_filter(&ToggleFilter, window, cx)
                        })),
                )
            })
            .when(pending > 0, |f| {
                f.child(div().w(px(8.)))
                    .child(
                        Button::new("discard")
                            .label("Discard")
                            .small()
                            .ghost()
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.discard(&Discard, window, cx)
                            })),
                    )
                    .child(
                        Button::new("preview")
                            .label("Preview SQL")
                            .small()
                            .ghost()
                            .on_click(
                                cx.listener(|this, _, window, cx| this.preview_sql(window, cx)),
                            ),
                    )
                    .child(
                        Button::new("commit")
                            .label(format!("Commit {pending}"))
                            .small()
                            .primary()
                            .tooltip("Commit (⌘S)")
                            .on_click(
                                cx.listener(|this, _, window, cx| this.commit(&Commit, window, cx)),
                            ),
                    )
            })
            .child(div().flex_1())
            .child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(info),
            )
            .child(div().w(px(8.)))
            .child(
                Button::new("prev-page")
                    .icon(IconName::ChevronLeft)
                    .ghost()
                    .small()
                    .disabled(page == 0)
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.reload_unless_dirty(window, cx, |t| t.page = t.page.saturating_sub(1));
                    })),
            )
            .child(div().text_xs().child(format!(
                "{} / {}",
                page + 1,
                pages.map(|p| p.to_string()).unwrap_or("?".into())
            )))
            .child(
                Button::new("next-page")
                    .icon(IconName::ChevronRight)
                    .ghost()
                    .small()
                    .disabled(pages.is_some_and(|p| page + 1 >= p))
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.reload_unless_dirty(window, cx, |t| t.page += 1);
                    })),
            )
            .child(
                Button::new("page-size")
                    .label(format!("{} / page", self.page_size))
                    .ghost()
                    .small()
                    .tooltip("Change page size")
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.reload_unless_dirty(window, cx, |t| {
                            let i = PAGE_SIZES
                                .iter()
                                .position(|s| *s == t.page_size)
                                .unwrap_or(0);
                            t.page_size = PAGE_SIZES[(i + 1) % PAGE_SIZES.len()];
                            t.page = 0;
                        });
                    })),
            )
            .child(
                Button::new("refresh")
                    .icon(IconName::RefreshCw)
                    .ghost()
                    .small()
                    .tooltip("Refresh (⌘R)")
                    .on_click(cx.listener(|this, _, window, cx| this.refresh(window, cx))),
            )
    }
}

impl copy::ExportHost for TableTab {
    fn export_grid(&self) -> &Entity<TableState<Grid>> {
        &self.table
    }

    fn sql_target(&self, _cx: &App) -> crate::export::SqlTarget {
        crate::export::SqlTarget {
            driver: self.shared.cfg.driver,
            schema: self.object.schema.clone(),
            table: self.object.name.clone(),
        }
    }

    fn export_name(&self) -> String {
        self.object.name.clone()
    }

    fn export_source(&self, cx: &App) -> copy::Source {
        copy::Source::Table {
            conn: self.shared.conn.clone(),
            query: self.current_query(cx),
        }
    }
}

impl cell_edit::CellEditHost for TableTab {
    fn grid(&self) -> &Entity<TableState<Grid>> {
        &self.table
    }

    fn edit_sub(&mut self) -> &mut Option<Subscription> {
        &mut self.edit_sub
    }
}

impl Focusable for TableTab {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for TableTab {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let body = if self.show_structure {
            let section = self.struct_section;
            let ddl = self
                .structure
                .as_ref()
                .map(|s| s.ddl.clone())
                .unwrap_or_default();
            let content = match section {
                0 => v_flex()
                    .size_full()
                    .when(
                        !matches!(self.shared.cfg.driver, crate::db::Driver::Redis | crate::db::Driver::Mongo)
                            && self.object.kind == crate::db::ObjectKind::Table,
                        |d| {
                            d.child(
                                h_flex().p_1().child(
                                    Button::new("edit-columns")
                                        .icon(IconName::Replace)
                                        .label("Edit Columns…")
                                        .ghost()
                                        .xsmall()
                                        .on_click(cx.listener(|t, _, window, cx| {
                                            t.edit_columns(window, cx)
                                        })),
                                ),
                            )
                        },
                    )
                    .child(div().flex_1().min_h_0().child(data_table(&self.struct_cols, cx)))
                    .into_any_element(),
                1 => data_table(&self.struct_idx, cx).into_any_element(),
                2 => data_table(&self.struct_fks, cx).into_any_element(),
                _ => v_flex()
                    .size_full()
                    .child(
                        h_flex()
                            .gap_1()
                            .p_1()
                            .child(
                                Button::new("copy-ddl")
                                    .icon(IconName::Copy)
                                    .label("Copy")
                                    .ghost()
                                    .xsmall()
                                    .on_click({
                                        let ddl = ddl.clone();
                                        move |_, window, cx| {
                                            cx.write_to_clipboard(ClipboardItem::new_string(
                                                ddl.clone(),
                                            ));
                                            window.push_notification("SQL copied", cx);
                                        }
                                    }),
                            )
                            .child(
                                Button::new("ddl-to-query")
                                    .icon(IconName::SquareTerminal)
                                    .label("Open in Query Tab")
                                    .ghost()
                                    .xsmall()
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        let sql = this
                                            .structure
                                            .as_ref()
                                            .map(|s| s.ddl.clone())
                                            .unwrap_or_default();
                                        cx.emit(TableTabEvent::OpenQuery(sql));
                                    })),
                            ),
                    )
                    .child(
                        div()
                            .id("ddl")
                            .flex_1()
                            .min_h_0()
                            .overflow_scroll()
                            .p_2()
                            .text_sm()
                            .font_family(theme.mono_font_family.clone())
                            .child(if ddl.is_empty() {
                                "-- Not available for this object".to_string()
                            } else {
                                ddl
                            }),
                    )
                    .into_any_element(),
            };
            let fk_count = self.structure.as_ref().map_or(0, |s| s.foreign_keys.len());
            let idx_count = self.structure.as_ref().map_or(0, |s| s.indexes.rows.len());
            v_flex()
                .size_full()
                .child(
                    TabBar::new("structure-sections")
                        .underline()
                        .small()
                        .selected_index(section)
                        .child(Tab::new().label("Columns"))
                        .child(Tab::new().label(format!("Indexes ({idx_count})")))
                        .child(Tab::new().label(format!("Foreign Keys ({fk_count})")))
                        .child(Tab::new().label("SQL"))
                        .on_click(cx.listener(|this, ix: &usize, _, cx| {
                            this.struct_section = *ix;
                            cx.notify();
                        })),
                )
                .children(self.structure.iter().flat_map(|s| s.warnings.clone()).map(|w| {
                    div()
                        .px_2()
                        .py_1()
                        .text_xs()
                        .bg(cx.theme().warning.opacity(0.15))
                        .child(format!("Couldn't load {w}"))
                }))
                .child(div().flex_1().min_h_0().child(content))
                .into_any_element()
        } else {
            let grid = cell_edit::editable(data_table(&self.table, cx), cx);
            if self.show_detail {
                let types: Vec<Option<String>> = {
                    let cols = &self.table.read(cx).delegate().columns;
                    cols.iter()
                        .map(|c| {
                            self.structure
                                .as_ref()?
                                .columns
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
        v_flex()
            .size_full()
            .key_context("TableTab")
            .track_focus(&self.focus)
            .on_action(cx.listener(|this, _: &CopyCell, window, cx| this.copy_cell(window, cx)))
            .on_action(cx.listener(|this, _: &SetNull, window, cx| this.set_null(window, cx)))
            .when(!self.fixed.is_empty() && !self.show_structure, |d| {
                let text = self
                    .fixed
                    .iter()
                    .map(|f| format!("{} {} {}", f.column, f.op.label(), f.value))
                    .collect::<Vec<_>>()
                    .join(" AND ");
                d.child(
                    h_flex()
                        .gap_1()
                        .px_2()
                        .py_1()
                        .border_b_1()
                        .border_color(cx.theme().border)
                        .text_xs()
                        .child(
                            h_flex()
                                .gap_1()
                                .pl_2()
                                .rounded_md()
                                .bg(cx.theme().accent)
                                .child(format!("Referenced row: {text}"))
                                .child(
                                    Button::new("clear-fixed")
                                        .debug_selector(|| "clear-fixed".into())
                                        .icon(IconName::Close)
                                        .ghost()
                                        .xsmall()
                                        .tooltip("Show all rows")
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.reload_unless_dirty(window, cx, |t| {
                                                t.fixed.clear();
                                                t.page = 0;
                                            });
                                        })),
                                ),
                        ),
                )
            })
            .when(self.show_filters && !self.show_structure, |d| {
                d.child(self.render_filters(cx))
            })
            .when_some(self.error.clone(), |d, e| {
                d.child(
                    div()
                        .p_2()
                        .text_sm()
                        .bg(cx.theme().danger.opacity(0.12))
                        .text_color(cx.theme().danger)
                        .child(e),
                )
            })
            .child(div().flex_1().min_h_0().child(body))
            .child(self.render_footer(cx))
    }
}
