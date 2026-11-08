//! A table / collection / key-space tab: Data view (filter bar, editable grid,
//! pager) and Structure view (columns + indexes).

use std::rc::Rc;
use std::time::Instant;

use anyhow::Context as _;

use gpui::{
    App, ClipboardItem, Context, Entity, FocusHandle, Focusable, FontWeight, IntoElement,
    ParentElement, Render, Styled, Subscription, Window, div, prelude::*, px,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::{EditorState, Input, InputEvent, InputState};
use gpui_component::menu::DropdownMenu as _;
use gpui_component::menu::{PopupMenu, PopupMenuItem};
use gpui_component::notification::Notification;
use gpui_component::select::{Select, SelectEvent, SelectState};
use gpui_component::tab::{Tab, TabBar};
use gpui_component::table::{TableEvent, TableState};
use gpui_component::{
    ActiveTheme as _, Icon, IndexPath, Selectable as _, Sizable as _, WindowExt as _, h_flex,
    v_flex,
};
use gpui_kit_assets::IconName;

use super::cell_edit::{self, EditStart};
use super::copy;
use super::dialog;
#[cfg(test)]
use super::foreign_key_editor::confirm_drop as confirm_drop_foreign_key;
use super::foreign_key_editor::{ForeignKeyDialog, render_list as render_foreign_key_list};
use super::grid::{Grid, Kind, data_table};
#[cfg(test)]
use super::index_editor::confirm_drop as confirm_drop_index;
use super::index_editor::{IndexDialog, render_list as render_index_list};
use super::loading;
use super::mongo_views::{DocView, json_editor, page_json, render_documents, render_json};
use super::redis_key_view::RedisKeyView;
use super::row_panel::{self, InboundLink};
use super::structure_editor::StructureEditor;
use super::table_editor::OnDone;
use super::theme::{dp, tokens};
use super::{
    AddRow, Commit, CopyCell, DeleteRows, Discard, ExportData, SearchList, SetNull, Shared,
    ToggleFilter, search_list,
};
use crate::db::binary::{decode, encode};
use crate::db::ddl::index_entries;
use crate::db::{
    DbObject, Driver, Filter, FilterKind, FilterOp, ObjectKind, QueryResult, Structure, TableQuery,
};
use crate::runtime;
use crate::ui::DisabledWhen as _;
use anyhow::{Result, bail};
use dirs::{download_dir, home_dir};
use gpui::{AnyElement, Div, EventEmitter, KeyDownEvent, PathPromptOptions, Stateful};
use std::path::PathBuf;
use tokio::fs::{metadata as tokio_metadata, read, write as tokio_write};
use tokio::task::spawn_blocking;

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

impl EventEmitter<TableTabEvent> for TableTab {}

use crate::settings::PAGE_SIZES;

struct FilterRow {
    column: Entity<SelectState<SearchList>>,
    /// Only the operators that fit the column's type; swapped when the
    /// column changes.
    op: Entity<SelectState<Vec<&'static str>>>,
    value: Entity<InputState>,
    /// `between`'s upper bound.
    value2: Entity<InputState>,
}

pub struct TableTab {
    shared: Rc<Shared>,
    pub object: DbObject,
    table: Entity<TableState<Grid>>,
    structure: Option<Structure>,
    /// Structure › Columns, editable in place; rebuilt when the structure
    /// is read again.
    columns_editor: Option<Entity<StructureEditor>>,
    /// Redis: the selected key's editor, beside the key list.
    redis_key: Option<Entity<RedisKeyView>>,
    /// MongoDB: Documents (cards), Table or JSON.
    doc_view: DocView,
    /// MongoDB's find bar, and the filter document it applied.
    find_input: Option<Entity<InputState>>,
    find: String,
    /// MongoDB's projection: the fields input, and the fields it applied.
    fields_input: Option<Entity<InputState>>,
    fields: Vec<String>,
    /// The JSON view's editor and the text it was made from.
    json_view: Option<(Entity<EditorState>, String)>,
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
    /// The filter editor (⌘F) is open. Filters apply whether or not it is.
    show_filters: bool,
    /// Filters in effect for the rows shown, with their editor row: the
    /// toolbar's chips.
    applied: Vec<(usize, Filter)>,
    /// The filter editor's All / Any.
    match_any: bool,
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
    /// The last import dialog opened (tests reach it through this).
    import_view: Option<Entity<crate::ui::csv_import_view::CsvImportView>>,
    /// When the running load began; `None` when idle.
    load_started: Option<Instant>,
    /// The same for foreign-key lookups while editing.
    fk_seq: u64,
    /// Label columns of referenced tables, looked up once per table.
    fk_labels: std::collections::HashMap<String, Vec<String>>,
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
        grid.empty_view = Some(Rc::new(move |_, cx| {
            w.upgrade()
                .map(|t| TableTab::render_no_rows(&t, cx))
                .unwrap_or_else(|| div().into_any_element())
        }));
        let w = weak.clone();
        grid.on_menu = Some(Rc::new(move |row, menu, window, cx| {
            let item =
                |label: &'static str,
                 action: fn(&mut TableTab, usize, &mut Window, &mut Context<TableTab>)| {
                    let w = w.clone();
                    PopupMenuItem::new(label).on_click(move |_, window, cx| {
                        w.update(cx, |t, cx| action(t, row, window, cx)).ok();
                    })
                };
            // What's under the pointer: "orders #10356", and whether the
            // cell is a foreign key that can be followed.
            let inbound = w
                .upgrade()
                .map(|tab| tab.read(cx).inbound_labels())
                .unwrap_or_default();
            // The cell under the pointer holds bytes: offer Save / Load file.
            let binary_column = w.upgrade().and_then(|tab| {
                let table = tab.read(cx).table.read(cx);
                let column = table.selected_cell().map(|(_, column)| column)?;
                (table.delegate().kind(column) == Kind::Binary).then_some(column)
            });
            let (header, on_reference) = w
                .upgrade()
                .map(|tab| {
                    let tab = tab.read(cx);
                    let table = tab.table.read(cx);
                    let grid = table.delegate();
                    let key = grid
                        .pk
                        .iter()
                        .position(|is_key| *is_key)
                        .and_then(|column| grid.cell_string(row, column));
                    let column = table.selected_cell().map(|(_, c)| c);
                    let reference = column.and_then(|c| grid.refs.get(c).cloned().flatten());
                    let header = match key {
                        Some(key) => format!("{} #{key}", tab.object.name),
                        None => format!("{} · row {}", tab.object.name, row + 1),
                    };
                    (header, reference)
                })
                .unwrap_or_default();
            menu.label(header)
                .item(item("Copy Cell", |t, _, window, cx| {
                    t.copy_cell(window, cx)
                }))
                .map(|menu| copy::copy_items(menu, w.clone(), row, window, cx))
                .separator()
                .item(item("Row Details", |t, _, _, cx| {
                    t.show_detail = true;
                    cx.notify();
                }))
                .item(item("Edit Cell", |t, row, window, cx| {
                    let column = t
                        .table
                        .read(cx)
                        .selected_cell()
                        .map(|(_, c)| c)
                        .unwrap_or(0);
                    cell_edit::start_edit(t, row, column, EditStart::Keep, window, cx);
                }))
                .item(item("Set NULL", |t, _, window, cx| t.set_null(window, cx)))
                .item(item("Duplicate Row", |t, row, _, cx| {
                    t.duplicate_row(row, cx)
                }))
                .separator()
                .item(item("Filter by This Value", |t, row, window, cx| {
                    t.filter_by_cell(row, window, cx)
                }))
                .when_some(on_reference, |menu, table| {
                    let w = w.clone();
                    menu.item(
                        PopupMenuItem::new(format!("Open Referenced Row in {table}")).on_click(
                            move |_, _, cx| {
                                w.update(cx, |t, cx| {
                                    let column = t
                                        .table
                                        .read(cx)
                                        .selected_cell()
                                        .map(|(_, c)| c)
                                        .unwrap_or(0);
                                    t.follow(row, column, cx)
                                })
                                .ok();
                            },
                        ),
                    )
                })
                .when_some(binary_column, |menu, column| {
                    let (save, load) = (w.clone(), w.clone());
                    menu.separator()
                        .item(PopupMenuItem::new("Save Value to File…").on_click(
                            move |_, window, cx| {
                                save.update(cx, |t, cx| {
                                    t.save_cell_to_file(row, column, window, cx)
                                })
                                .ok();
                            },
                        ))
                        .item(PopupMenuItem::new("Load File into Cell…").on_click(
                            move |_, window, cx| {
                                load.update(cx, |t, cx| {
                                    t.load_file_into_cell(row, column, window, cx)
                                })
                                .ok();
                            },
                        ))
                })
                .when(!inbound.is_empty(), |menu| {
                    let w = w.clone();
                    let referencing = PopupMenu::build(window, cx, move |menu, _, _| {
                        inbound
                            .iter()
                            .enumerate()
                            .fold(menu, |menu, (index, label)| {
                                let w = w.clone();
                                menu.item(PopupMenuItem::new(label.clone()).on_click(
                                    move |_, _, cx| {
                                        w.update(cx, |t, cx| t.open_referencing(row, index, cx))
                                            .ok();
                                    },
                                ))
                            })
                    });
                    menu.item(PopupMenuItem::submenu("Rows Referencing This", referencing))
                })
                .item(item("Import CSV…", |t, _, window, cx| {
                    t.import_csv(window, cx)
                }))
                .separator()
                .item(item("Delete Row", |t, row, _, cx| {
                    t.table.update(cx, |table, cx| {
                        table.delegate_mut().toggle_delete(row);
                        cx.notify();
                    });
                    cx.notify();
                }))
        }));

        let w = weak.clone();
        grid.on_follow = Some(Rc::new(move |row, col, _, cx| {
            w.update(cx, |t, cx| t.follow(row, col, cx)).ok();
        }));
        let w = weak.clone();
        grid.on_fk_search = Some(Rc::new(move |col, text, _, cx| {
            w.update(cx, |t, cx| t.fk_search(col, text, cx)).ok();
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
                    // The row detail panel and the Redis key editor follow
                    // the selection.
                    TableEvent::SelectRow(_) | TableEvent::SelectCell(..) => {
                        this.follow_selected_key(window, cx);
                        cx.notify()
                    }
                    _ => {}
                }
            }),
        ];

        let mut this = Self {
            shared,
            object,
            table,
            structure: None,
            columns_editor: None,
            redis_key: None,
            doc_view: DocView::default(),
            find_input: None,
            find: String::new(),
            fields_input: None,
            fields: vec![],
            json_view: None,
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
            applied: vec![],
            match_any: false,
            page: 0,
            page_size: crate::settings::get(cx).page_size,
            total: None,
            elapsed: 0,
            error: None,
            focus: cx.focus_handle(),
            edit_sub: None,
            load_seq: 0,
            load_started: None,
            import_view: None,
            fk_seq: 0,
            fk_labels: Default::default(),
            _subs: subs,
        };
        if this.shared.cfg.driver == Driver::Mongo {
            let input = cx.new(|cx| {
                InputState::new(window, cx).placeholder("{ status: \"paid\", total: { $gt: 100 } }")
            });
            this._subs.push(cx.subscribe_in(
                &input,
                window,
                |tab, _, event: &InputEvent, _, cx| {
                    if let InputEvent::PressEnter { .. } = event {
                        tab.apply_find(cx);
                    }
                },
            ));
            this.find_input = Some(input);
            let fields = cx.new(|cx| InputState::new(window, cx).placeholder("all fields"));
            this._subs.push(cx.subscribe_in(
                &fields,
                window,
                |tab, _, event: &InputEvent, _, cx| {
                    if let InputEvent::PressEnter { .. } = event {
                        tab.apply_find(cx);
                    }
                },
            ));
            this.fields_input = Some(fields);
        }
        if this.shared.cfg.driver == Driver::Redis {
            let tab = cx.entity().downgrade();
            let on_changed = Rc::new(move |_: &mut Window, cx: &mut App| {
                tab.update(cx, |tab, cx| tab.reload(cx)).ok();
            });
            let shared = this.shared.clone();
            this.redis_key = Some(cx.new(|cx| RedisKeyView::new(shared, on_changed, window, cx)));
        }
        this.reload(cx);
        this
    }

    /// Runs the find bar's filter from the first page. Refused while edits
    /// are staged, like any reload.
    fn apply_find(&mut self, cx: &mut Context<Self>) {
        let Some(input) = &self.find_input else {
            return;
        };
        let find = input.read(cx).value().trim().to_string();
        // "name, status" → the projection; `_id` always comes along.
        let fields: Vec<String> = self
            .fields_input
            .as_ref()
            .map(|input| {
                input
                    .read(cx)
                    .value()
                    .split(',')
                    .map(|field| field.trim().to_string())
                    .filter(|field| !field.is_empty())
                    .collect()
            })
            .unwrap_or_default();
        if find == self.find && fields == self.fields {
            return;
        }
        if self.is_dirty(cx) {
            self.error = Some("Commit (⌘S) or discard (⌘⇧⌫) your changes first".into());
            cx.notify();
            return;
        }
        self.find = find;
        self.fields = fields;
        self.page = 0;
        self.reload(cx);
    }

    fn render_find_bar(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let input = self.find_input.as_ref()?;
        let t = tokens(cx);
        let mono = cx.theme().mono_font_family.clone();
        Some(
            h_flex()
                .flex_none()
                .gap(dp(8.))
                .px(dp(12.))
                .py(dp(6.))
                .border_b_1()
                .border_color(t.border)
                .child(
                    div()
                        .text_size(dp(12.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(t.text_muted)
                        .child("find"),
                )
                .child(
                    div()
                        .flex_1()
                        .font_family(mono.clone())
                        .child(Input::new(input).small().cleanable(true)),
                )
                .children(self.fields_input.as_ref().map(|fields| {
                    h_flex()
                        .gap(dp(8.))
                        .child(
                            div()
                                .text_size(dp(12.))
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(t.text_muted)
                                .child("fields"),
                        )
                        .child(
                            div()
                                .w(dp(220.))
                                .font_family(mono)
                                .child(Input::new(fields).small().cleanable(true)),
                        )
                }))
                .when(!self.find.is_empty() || !self.fields.is_empty(), |bar| {
                    bar.child(
                        div()
                            .text_size(dp(11.5))
                            .text_color(t.text_muted)
                            .child("↵ applies · clear and ↵ to show all"),
                    )
                })
                .into_any_element(),
        )
    }

    #[cfg(test)]
    pub fn error_for_test(&self) -> Option<&str> {
        self.error.as_deref()
    }

    #[cfg(test)]
    pub fn fields_for_test(&mut self, fields: &str, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(input) = self.fields_input.clone() {
            input.update(cx, |input, cx| {
                input.set_value(fields.to_string(), window, cx)
            });
        }
        self.apply_find(cx);
    }

    #[cfg(test)]
    pub fn find_for_test(&mut self, find: &str, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(input) = self.find_input.clone() {
            input.update(cx, |input, cx| {
                input.set_value(find.to_string(), window, cx)
            });
        }
        self.apply_find(cx);
    }

    /// The Redis key editor shows the selected row's key (rows fetched
    /// from the server only: a staged new key isn't there yet).
    fn follow_selected_key(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(view) = self.redis_key.clone() else {
            return;
        };
        let grid = self.table.read(cx).delegate();
        let key_column = grid.columns.iter().position(|column| column == "key");
        let key = self
            .selected_row(cx)
            .filter(|row| *row < grid.base_rows)
            .zip(key_column)
            .and_then(|(row, column)| grid.cell_string(row, column));
        view.update(cx, |view, cx| view.show_key(key, window, cx));
    }

    #[cfg(test)]
    pub fn set_doc_view_for_test(&mut self, view: DocView, cx: &mut Context<Self>) {
        self.show_structure = false;
        self.doc_view = view;
        cx.notify();
    }

    #[cfg(test)]
    pub fn doc_view(&self) -> DocView {
        self.doc_view
    }

    #[cfg(test)]
    pub fn page_json_for_test(&self, cx: &App) -> String {
        page_json(self.table.read(cx).delegate())
    }

    #[cfg(test)]
    pub fn redis_key_view(&self) -> Option<Entity<RedisKeyView>> {
        self.redis_key.clone()
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

    #[cfg(test)]
    pub fn set_filter_value_for_test(
        &mut self,
        i: usize,
        value: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.filters[i]
            .value
            .update(cx, |input, cx| input.set_value(value, window, cx));
        self.reload_unless_dirty(window, cx, |t| t.page = 0);
    }

    #[cfg(test)]
    pub fn filter_by_cell_for_test(
        &mut self,
        row: usize,
        column: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.table
            .update(cx, |table, cx| table.set_selected_cell(row, column, cx));
        self.filter_by_cell(row, window, cx);
    }

    #[cfg(test)]
    pub fn import_view_for_test(
        &self,
    ) -> Option<Entity<crate::ui::csv_import_view::CsvImportView>> {
        self.import_view.clone()
    }

    #[cfg(test)]
    pub fn grid_for_test(&self) -> Entity<TableState<Grid>> {
        self.table.clone()
    }

    /// Structure › Columns (the sidebar's Edit Columns…).
    pub fn show_columns(&mut self, cx: &mut Context<Self>) {
        self.show_structure = true;
        self.struct_section = 0;
        cx.notify();
    }

    #[cfg(test)]
    pub fn show_foreign_keys_for_test(&mut self, cx: &mut Context<Self>) {
        self.show_structure = true;
        self.struct_section = 2;
        cx.notify();
    }

    #[cfg(test)]
    pub fn show_indexes_for_test(&mut self, cx: &mut Context<Self>) {
        self.show_structure = true;
        self.struct_section = 1;
        cx.notify();
    }

    #[cfg(test)]
    pub fn show_data_for_test(&mut self, cx: &mut Context<Self>) {
        self.show_structure = false;
        cx.notify();
    }

    /// For screenshots: two conditions, "any", the editor open.
    #[cfg(test)]
    pub fn show_filter_example(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.show_filters = true;
        self.filters.clear();
        self.add_filter(window, cx);
        self.add_filter(window, cx);
        self.set_filter_row(0, "status", FilterOp::In, "paid, shipped", window, cx);
        self.set_filter_row(1, "total", FilterOp::Between, "500", window, cx);
        self.filters[1]
            .value2
            .update(cx, |input, cx| input.set_value("1500", window, cx));
        self.match_any = true;
        self.reload_unless_dirty(window, cx, |t| t.page = 0);
    }

    #[cfg(test)]
    pub fn fk_matches_for_test(&self, cx: &App) -> Option<Vec<String>> {
        let g = self.table.read(cx).delegate();
        g.fk_matches
            .as_ref()
            .map(|m| m.iter().map(|(k, _)| k.clone()).collect())
    }

    #[cfg(test)]
    pub fn chips_for_test(&self) -> usize {
        self.applied.len()
    }

    #[cfg(test)]
    pub fn remove_filter_for_test(
        &mut self,
        i: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.remove_filter(i, window, cx);
    }

    pub fn title(&self) -> String {
        self.object.title()
    }

    /// For the status bar: "orders · 312 rows · 18 ms".
    pub fn status_line(&self, cx: &App) -> String {
        let shown = self.table.read(cx).delegate().base_rows;
        let rows = self.total.unwrap_or(shown as u64);
        let rows = if rows == 1 {
            "1 row".to_string()
        } else {
            format!("{rows} rows")
        };
        format!("{} · {rows} · {} ms", self.title(), self.elapsed)
    }

    pub fn is_dirty(&self, cx: &App) -> bool {
        self.table.read(cx).delegate().is_dirty()
    }

    fn filter_values(&self, cx: &App) -> Vec<Filter> {
        self.filter_rows(cx).into_iter().map(|(_, f)| f).collect()
    }

    /// Complete filters with the index of their editor row.
    fn filter_rows(&self, cx: &App) -> Vec<(usize, Filter)> {
        self.filters
            .iter()
            .enumerate()
            .filter_map(|(i, f)| {
                let column = f.column.read(cx).selected_value()?.clone();
                let op_label = *f.op.read(cx).selected_value()?;
                let op = FilterOp::ALL.into_iter().find(|o| o.label() == op_label)?;
                let value = f.value.read(cx).value().to_string();
                let value2 = f.value2.read(cx).value().to_string();
                if op.needs_value() && value.is_empty() {
                    return None;
                }
                if op == FilterOp::Between && value2.is_empty() {
                    return None;
                }
                Some((
                    i,
                    Filter {
                        column,
                        op,
                        value,
                        value2,
                    },
                ))
            })
            .collect()
    }

    /// A chip's ×: drop that filter and show the rows again.
    fn remove_filter(&mut self, row: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.reload_unless_dirty(window, cx, |t| {
            if row < t.filters.len() {
                t.filters.remove(row);
            }
            if t.filters.is_empty() {
                t.show_filters = false;
            }
            t.page = 0;
        });
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
                .chain(self.filter_values(cx))
                .collect(),
            order,
            limit: self.page_size,
            offset: self.page * self.page_size,
            // A followed foreign key always applies, so "any" only counts
            // without one.
            match_any: self.match_any && self.fixed.is_empty(),
            find: self.find.clone(),
            fields: self.fields.clone(),
        }
    }

    pub fn reload(&mut self, cx: &mut Context<Self>) {
        let q = self.current_query(cx);
        self.applied = self.filter_rows(cx);
        self.table.update(cx, |t, cx| {
            t.delegate_mut().set_loading(true);
            cx.notify();
        });
        let conn = self.shared.conn.clone();
        let obj = self.object.clone();
        let need_structure = self.structure.is_none();
        self.load_seq += 1;
        let seq = self.load_seq;
        if self.load_started.is_none() {
            loading::tick(cx, |t: &Self| t.load_started.is_some());
        }
        self.load_started = Some(Instant::now());
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
        res: Result<(Option<Structure>, (QueryResult, Option<u64>))>,
        cx: &mut Context<Self>,
    ) {
        self.load_started = None;
        self.table.update(cx, |t, cx| {
            t.delegate_mut().set_loading(false);
            cx.notify();
        });
        let (st, (data, total)) = match res {
            Ok(v) => v,
            Err(e) => {
                self.error = Some(format!("{e:#}"));
                self.shared.report(&format!("{e:#}"));
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
            self.columns_editor = None;
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
        let table_cols = self.structure.as_ref().map(|s| s.columns.clone());
        // (column, referenced table) for every foreign-key column.
        let fk_refs: Vec<(String, String)> = self
            .structure
            .iter()
            .flat_map(|s| &s.foreign_keys)
            .flat_map(|f| f.columns.iter().map(|c| (c.clone(), f.ref_table.clone())))
            .collect();
        self.table.update(cx, |t, cx| {
            let g = t.delegate_mut();
            g.set_result(&data);
            g.pk = g.columns.iter().map(|c| pk_names.contains(c)).collect();
            g.links = g
                .columns
                .iter()
                .map(|c| fk_refs.iter().any(|(f, _)| f == c))
                .collect();
            g.set_choices(table_cols.as_deref().unwrap_or_default());
            g.set_types(table_cols.as_deref().unwrap_or_default(), &fk_refs);
            g.editable = vec![true; g.columns.len()];
            t.refresh(cx);
            cx.notify();
        });
        cx.notify();
    }

    /// The empty grid (design.md, screen 22): no rows at all, or none that
    /// match the filters, each with the action that helps.
    fn render_no_rows(this: &Entity<Self>, cx: &mut App) -> AnyElement {
        let t = tokens(cx);
        let tab = this.read(cx);
        let filters = tab.applied.len() + tab.fixed.len();
        let name = tab.object.name.clone();
        let view = tab.object.kind == ObjectKind::View;
        let (title, detail) = if filters > 0 {
            (
                format!(
                    "No rows match {}",
                    if filters == 1 {
                        "the filter".to_string()
                    } else {
                        format!("{filters} filters")
                    }
                ),
                tab.applied
                    .iter()
                    .map(|(_, f)| f)
                    .chain(tab.fixed.iter())
                    .map(|f| {
                        match f.op {
                            FilterOp::Between => {
                                format!("{} between {} and {}", f.column, f.value, f.value2)
                            }
                            op => format!("{} {} {}", f.column, op.label(), f.value),
                        }
                        .trim()
                        .to_string()
                    })
                    .collect::<Vec<_>>()
                    .join(" · "),
            )
        } else if view {
            (format!("{name} returns no rows"), String::new())
        } else {
            (format!("{name} has no rows yet"), String::new())
        };
        let me = this.downgrade();
        let actions = if filters > 0 {
            h_flex().child(
                Button::new("clear-all-filters")
                    .cursor_pointer()
                    .label("Clear filters")
                    .outline()
                    .small()
                    .on_click(move |_, window, cx| {
                        me.update(cx, |t, cx| t.clear_filters(window, cx)).ok();
                    }),
            )
        } else if !view {
            let me2 = me.clone();
            h_flex()
                .gap(dp(8.))
                .child(
                    Button::new("empty-add-row")
                        .cursor_pointer()
                        .icon(IconName::Plus)
                        .label("Add row ⌘I")
                        .primary()
                        .small()
                        .on_click(move |_, window, cx| {
                            me.update(cx, |t, cx| t.add_row(&AddRow, window, cx)).ok();
                        }),
                )
                .child(
                    Button::new("empty-import")
                        .cursor_pointer()
                        .label("Import CSV…")
                        .outline()
                        .small()
                        .on_click(move |_, window, cx| {
                            me2.update(cx, |t, cx| t.import_csv(window, cx)).ok();
                        }),
                )
        } else {
            h_flex()
        };
        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .gap(dp(8.))
            .child(
                Icon::new(if filters > 0 {
                    IconName::Funnel
                } else {
                    IconName::Table
                })
                .text_color(t.text_faint),
            )
            .child(
                div()
                    .text_size(dp(14.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(t.text)
                    .child(title),
            )
            .when(!detail.is_empty(), |d| {
                d.child(
                    div()
                        .max_w(dp(520.))
                        .text_center()
                        .font_family(cx.theme().mono_font_family.clone())
                        .text_size(dp(11.5))
                        .text_color(t.text_muted)
                        .child(detail),
                )
            })
            .child(actions.pt(dp(6.)))
            .into_any_element()
    }

    #[cfg(test)]
    pub fn is_loading(&self, cx: &App) -> bool {
        self.loading(cx)
    }

    /// For screenshots: a load that has been running for a second.
    #[cfg(test)]
    pub fn pretend_loading(&mut self, cx: &mut Context<Self>) {
        let since = Instant::now() - std::time::Duration::from_secs(1);
        self.load_started = Some(since);
        self.table.update(cx, |t, cx| {
            let g = t.delegate_mut();
            g.loading = true;
            g.loading_since = Some(since);
            cx.notify();
        });
        cx.notify();
    }

    /// For screenshots: the tab with filters that match nothing.
    #[cfg(test)]
    pub fn show_no_matches(&mut self, cx: &mut Context<Self>) {
        self.load_started = None;
        self.fixed = vec![Filter {
            column: "status".into(),
            op: FilterOp::Eq,
            value: "archived".into(),
            value2: String::new(),
        }];
        self.applied.clear();
        self.table.update(cx, |t, cx| {
            let g = t.delegate_mut();
            g.set_loading(false);
            let cols = g.columns.clone();
            g.set_result(&QueryResult {
                columns: cols,
                ..Default::default()
            });
            t.refresh(cx);
        });
        cx.notify();
    }

    /// Drops every filter (bar and chips, and a followed foreign key's).
    pub fn clear_filters(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.reload_unless_dirty(window, cx, |t| {
            t.filters.clear();
            t.fixed.clear();
            t.show_filters = false;
            t.page = 0;
        });
    }

    /// Stop on the loading chip: drops the running load (its answer is
    /// ignored when it comes) and says how to load again.
    fn stop_load(&mut self, cx: &mut Context<Self>) {
        self.load_seq += 1;
        self.load_started = None;
        self.table.update(cx, |t, cx| {
            let g = t.delegate_mut();
            g.set_loading(false);
            g.set_result(&QueryResult::default());
            t.refresh(cx);
        });
        self.error = Some(format!(
            "Stopped loading {}. Refresh (⌘R) to load it again.",
            self.object.name
        ));
        cx.notify();
    }

    /// After the columns changed: fetch the structure again with the rows.
    pub fn reload_structure(&mut self, cx: &mut Context<Self>) {
        self.structure = None;
        self.reload(cx);
    }

    /// Tables on SQL servers; views, Redis and MongoDB stay read-only.
    fn structure_editable(&self) -> bool {
        !matches!(self.shared.cfg.driver, Driver::Redis | Driver::Mongo)
            && self.object.kind == ObjectKind::Table
    }

    /// Reads the structure again after a schema change.
    fn on_structure_change(&self, cx: &mut Context<Self>) -> OnDone {
        let tab = cx.entity().downgrade();
        Rc::new(move |_, cx| {
            tab.update(cx, |tab, cx| tab.reload_structure(cx)).ok();
        })
    }

    /// `+ Index`: the dialog, with this table's columns to pick from.
    pub fn open_index_dialog(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Entity<IndexDialog>> {
        let columns = self
            .structure
            .as_ref()?
            .columns
            .iter()
            .map(|column| column.name.clone())
            .collect();
        let on_done = self.on_structure_change(cx);
        Some(IndexDialog::open(
            self.shared.clone(),
            self.object.clone(),
            columns,
            on_done,
            window,
            cx,
        ))
    }

    #[cfg(test)]
    pub fn index_names(&self) -> Vec<String> {
        self.structure
            .as_ref()
            .map(|structure| index_entries(self.shared.cfg.driver, &structure.indexes))
            .unwrap_or_default()
            .into_iter()
            .map(|entry| entry.name)
            .collect()
    }

    #[cfg(test)]
    pub fn drop_index_for_test(&mut self, name: &str, window: &mut Window, cx: &mut Context<Self>) {
        let on_done = self.on_structure_change(cx);
        confirm_drop_index(self.shared.clone(), &self.object, name, on_done, window, cx);
    }

    /// `+ Foreign key`: the dialog, with this table's columns to pick from.
    pub fn open_foreign_key_dialog(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Entity<ForeignKeyDialog>> {
        let columns = self
            .structure
            .as_ref()?
            .columns
            .iter()
            .map(|column| column.name.clone())
            .collect();
        let on_done = self.on_structure_change(cx);
        Some(ForeignKeyDialog::open(
            self.shared.clone(),
            self.object.clone(),
            columns,
            on_done,
            window,
            cx,
        ))
    }

    fn render_foreign_keys(&self, cx: &mut Context<Self>) -> AnyElement {
        let keys = self
            .structure
            .as_ref()
            .map(|structure| structure.foreign_keys.clone())
            .unwrap_or_default();
        let tab = cx.entity().downgrade();
        let on_add = Rc::new(move |window: &mut Window, cx: &mut App| {
            tab.update(cx, |tab, cx| {
                tab.open_foreign_key_dialog(window, cx);
            })
            .ok();
        });
        let on_done = self.on_structure_change(cx);
        render_foreign_key_list(&self.shared, &self.object, &keys, on_add, on_done, cx)
    }

    #[cfg(test)]
    pub fn foreign_key_names(&self) -> Vec<String> {
        self.structure
            .iter()
            .flat_map(|structure| structure.foreign_keys.iter().map(|key| key.name.clone()))
            .collect()
    }

    #[cfg(test)]
    pub fn drop_foreign_key_for_test(
        &mut self,
        name: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let on_done = self.on_structure_change(cx);
        confirm_drop_foreign_key(self.shared.clone(), &self.object, name, on_done, window, cx);
    }

    fn render_indexes(&self, cx: &mut Context<Self>) -> AnyElement {
        let entries = self
            .structure
            .as_ref()
            .map(|structure| index_entries(self.shared.cfg.driver, &structure.indexes))
            .unwrap_or_default();
        let tab = cx.entity().downgrade();
        let on_add = Rc::new(move |window: &mut Window, cx: &mut App| {
            tab.update(cx, |tab, cx| {
                tab.open_index_dialog(window, cx);
            })
            .ok();
        });
        let on_done = self.on_structure_change(cx);
        render_index_list(&self.shared, &self.object, &entries, on_add, on_done, cx)
    }

    /// The in-place column editor, made once the structure is known (it
    /// needs the window for its inputs). Views, Redis and MongoDB keep the
    /// read-only list.
    fn ensure_columns_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.structure_editable() || self.columns_editor.is_some() {
            return;
        }
        let Some(structure) = &self.structure else {
            return;
        };
        let on_done = self.on_structure_change(cx);
        let (shared, object, columns) = (
            self.shared.clone(),
            self.object.clone(),
            structure.columns.clone(),
        );
        self.columns_editor =
            Some(cx.new(|cx| StructureEditor::new(shared, object, columns, on_done, window, cx)));
    }

    #[cfg(test)]
    pub fn columns_editor_for_test(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Entity<StructureEditor>> {
        self.show_structure = true;
        self.struct_section = 0;
        self.ensure_columns_editor(window, cx);
        self.columns_editor.clone()
    }

    /// While a foreign-key cell is edited: up to 5 rows of the referenced
    /// table whose key starts with `text` or whose first text columns
    /// contain it, into the grid's match list. Only the latest reply counts.
    fn fk_search(&mut self, col: usize, text: String, cx: &mut Context<Self>) {
        let g = self.table.read(cx).delegate();
        let Some(column) = g.columns.get(col).cloned() else {
            return;
        };
        let Some(fk) = self.structure.as_ref().and_then(|st| {
            st.foreign_keys
                .iter()
                .filter(|f| f.columns.contains(&column))
                .min_by_key(|f| f.columns.len())
                .cloned()
        }) else {
            return;
        };
        let Some(key) = fk
            .columns
            .iter()
            .position(|c| *c == column)
            .and_then(|i| fk.ref_columns.get(i).cloned())
        else {
            return;
        };
        let target = DbObject {
            schema: fk.ref_schema.clone().filter(|s| !s.is_empty()),
            name: fk.ref_table.clone(),
            kind: crate::db::ObjectKind::Table,
        };
        self.fk_seq += 1;
        let seq = self.fk_seq;
        let conn = self.shared.conn.clone();
        let cache_key = target.title();
        let cached = self.fk_labels.get(&cache_key).cloned();
        let task = runtime::spawn(async move {
            let c = conn.read().await;
            // Labels: the first two text columns other than the key.
            let labels = match cached {
                Some(l) => l,
                None => c
                    .structure(&target)
                    .await?
                    .columns
                    .iter()
                    .filter(|c| c.name != key)
                    .filter(|c| {
                        let t = c.data_type.to_ascii_lowercase();
                        t.contains("char") || t.contains("text")
                    })
                    .take(2)
                    .map(|c| c.name.clone())
                    .collect(),
            };
            let matches = c.fk_matches(&target, &key, &labels, &text, 5).await?;
            anyhow::Ok((labels, matches))
        });
        cx.spawn(async move |this, cx| {
            let res = task.await;
            this.update(cx, |this, cx| {
                if seq != this.fk_seq {
                    return;
                }
                let matches = match res {
                    Ok((labels, matches)) => {
                        this.fk_labels.insert(cache_key, labels);
                        matches
                    }
                    // A failed lookup just shows no matches.
                    Err(_) => vec![],
                };
                this.table.update(cx, |t, cx| {
                    let g = t.delegate_mut();
                    // Only while that cell is still being edited.
                    if g.editing.as_ref().is_some_and(|(_, c, _)| *c == col) {
                        g.fk_matches = Some(matches);
                        cx.notify();
                    }
                });
            })
            .ok();
        })
        .detach();
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
                value2: String::new(),
            });
        }
        cx.emit(TableTabEvent::OpenReference {
            schema: fk.ref_schema.clone().filter(|s| !s.is_empty()),
            table: fk.ref_table.clone(),
            filters,
        });
    }

    /// Writes a binary cell's bytes (its hex decoded) to a file the user
    /// picks.
    pub fn save_cell_to_file(
        &mut self,
        row: usize,
        column: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let grid = self.table.read(cx).delegate();
        let Some(text) = grid.cell_string(row, column) else {
            window.push_notification("The value is NULL: nothing to save", cx);
            return;
        };
        let bytes = decode(&text).unwrap_or_else(|| text.into_bytes());
        let name = format!(
            "{}-{}.bin",
            self.object.name,
            grid.columns.get(column).cloned().unwrap_or_default()
        );
        let directory = download_dir()
            .or_else(home_dir)
            .unwrap_or_else(|| PathBuf::from("."));
        let picked = cx.prompt_for_new_path(&directory, Some(&name));
        cx.spawn_in(window, async move |_, cx| {
            let Ok(Ok(Some(path))) = picked.await else {
                return;
            };
            let written = runtime::spawn(async move {
                tokio_write(&path, bytes)
                    .await
                    .with_context(|| format!("couldn't write {}", path.display()))?;
                anyhow::Ok(path)
            })
            .await;
            cx.update(|window, cx| match written {
                Ok(path) => window.push_notification(format!("Saved to {}", path.display()), cx),
                Err(error) => {
                    window.push_notification(Notification::error(format!("{error:#}")), cx)
                }
            })
            .ok();
        })
        .detach();
    }

    /// Stages a file's bytes as the binary cell's new value (hex, which
    /// the commit writes as bytes). Files over 16 MB are refused.
    pub fn load_file_into_cell(
        &mut self,
        row: usize,
        column: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let picked = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Load into cell".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = picked.await else {
                return;
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };
            this.update_in(cx, |tab, window, cx| {
                tab.stage_file(row, column, path, window, cx)
            })
            .ok();
        })
        .detach();
    }

    /// Reads `path` and stages its bytes in the cell.
    pub fn stage_file(
        &mut self,
        row: usize,
        column: usize,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        const LIMIT: u64 = 16 * 1024 * 1024;
        let driver = self.shared.cfg.driver;
        cx.spawn_in(window, async move |this, cx| {
            let read = runtime::spawn(async move {
                let size = tokio_metadata(&path).await?.len();
                if size > LIMIT {
                    bail!(
                        "{} is {} MB; cells take files up to 16 MB",
                        path.display(),
                        size / 1024 / 1024
                    );
                }
                anyhow::Ok(read(&path).await?)
            })
            .await;
            this.update_in(cx, |tab, window, cx| match read {
                Ok(bytes) => {
                    let value = encode(driver, &bytes);
                    tab.table.update(cx, |table, cx| {
                        table.delegate_mut().set_cell(row, column, Some(value));
                        table.refresh(cx);
                    });
                    cx.notify();
                }
                Err(error) => {
                    window.push_notification(Notification::error(format!("{error:#}")), cx)
                }
            })
            .ok();
        })
        .detach();
    }

    /// "orders (customer_id)" for each table whose foreign key points here.
    pub fn inbound_labels(&self) -> Vec<String> {
        self.structure
            .iter()
            .flat_map(|structure| &structure.referenced_by)
            .map(|key| format!("{} ({})", key.ref_table, key.ref_columns.join(", ")))
            .collect()
    }

    /// Opens the rows of the `index`th referencing table that point at
    /// `row` (a NULL in our key means nothing can).
    pub fn open_referencing(&mut self, row: usize, index: usize, cx: &mut Context<Self>) {
        let Some(key) = self
            .structure
            .as_ref()
            .and_then(|structure| structure.referenced_by.get(index))
        else {
            return;
        };
        let grid = self.table.read(cx).delegate();
        let mut filters = vec![];
        for (ours, theirs) in key.columns.iter().zip(&key.ref_columns) {
            let Some(value) = grid
                .columns
                .iter()
                .position(|column| column == ours)
                .and_then(|column| grid.cell_string(row, column))
            else {
                return;
            };
            filters.push(Filter {
                column: theirs.clone(),
                op: FilterOp::Eq,
                value,
                value2: String::new(),
            });
        }
        cx.emit(TableTabEvent::OpenReference {
            schema: key.ref_schema.clone().filter(|schema| !schema.is_empty()),
            table: key.ref_table.clone(),
            filters,
        });
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
        cell_edit::set_null(self, cx);
    }

    /// A page is loading; staged edits would be dropped when it arrives.
    fn loading(&self, cx: &App) -> bool {
        self.table.read(cx).delegate().loading
    }

    fn duplicate_row(&mut self, row: usize, cx: &mut Context<Self>) {
        if self.loading(cx) {
            return;
        }
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
        if self.show_structure || self.loading(cx) {
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

    /// ⌘F: show or hide the filter editor. Hiding keeps the filters (they
    /// stay as chips); a chip's × removes one.
    pub fn toggle_filter(&mut self, _: &ToggleFilter, window: &mut Window, cx: &mut Context<Self>) {
        if self.show_filters {
            self.show_filters = false;
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

    /// Points editor row `index` at `column` `op` `value`.
    fn set_filter_row(
        &self,
        index: usize,
        column: &str,
        op: FilterOp,
        value: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(row) = self.filters.get(index) else {
            return;
        };
        let columns = self.table.read(cx).delegate().columns.clone();
        let column_index = columns.iter().position(|c| c == column).unwrap_or(0);
        row.column.update(cx, |state, cx| {
            state.set_selected_index(Some(IndexPath::new(column_index)), window, cx)
        });
        let ops = self.ops_for(column);
        let op_index = ops.iter().position(|o| *o == op.label()).unwrap_or(0);
        row.op.update(cx, |state, cx| {
            state.set_items(ops, window, cx);
            state.set_selected_index(Some(IndexPath::new(op_index)), window, cx)
        });
        row.value.update(cx, |input, cx| {
            input.set_value(value.to_string(), window, cx)
        });
    }

    /// Row menu "Filter by this value": a condition on the cell's column
    /// (`is null` for NULL), applied right away.
    fn filter_by_cell(&mut self, row: usize, window: &mut Window, cx: &mut Context<Self>) {
        let table = self.table.read(cx);
        let column = table.selected_cell().map(|(_, c)| c).unwrap_or(0);
        let grid = table.delegate();
        let Some(name) = grid.columns.get(column).cloned() else {
            return;
        };
        let value = grid.cell_string(row, column);
        if self.is_dirty(cx) {
            window.push_notification(
                Notification::warning("Commit (⌘S) or discard (⌘⇧⌫) your changes first"),
                cx,
            );
            return;
        }
        self.add_filter(window, cx);
        let index = self.filters.len() - 1;
        match value {
            Some(value) => self.set_filter_row(index, &name, FilterOp::Eq, &value, window, cx),
            None => self.set_filter_row(index, &name, FilterOp::IsNull, "", window, cx),
        }
        self.reload_unless_dirty(window, cx, |t| t.page = 0);
    }

    /// The operators offered for `column` (design.md, answer #18).
    fn ops_for(&self, column: &str) -> Vec<&'static str> {
        let data_type = self
            .structure
            .as_ref()
            .and_then(|s| s.columns.iter().find(|c| c.name == column))
            .map(|c| c.data_type.clone())
            .unwrap_or_default();
        FilterOp::for_kind(FilterKind::of(&data_type), self.shared.cfg.driver)
            .into_iter()
            .map(|op| op.label())
            .collect()
    }

    fn add_filter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let columns: Vec<String> = self.table.read(cx).delegate().columns.clone();
        let first = columns.first().cloned().unwrap_or_default();
        let column = cx.new(|cx| {
            SelectState::new(search_list(columns), Some(IndexPath::new(0)), window, cx)
                .searchable(true)
        });
        let ops = self.ops_for(&first);
        let op = cx.new(|cx| SelectState::new(ops, Some(IndexPath::new(0)), window, cx));
        let value = cx.new(|cx| InputState::new(window, cx).placeholder("Value — ↵ applies"));
        let value2 = cx.new(|cx| InputState::new(window, cx).placeholder("upper bound"));
        for input in [&value, &value2] {
            self._subs.push(cx.subscribe_in(
                input,
                window,
                |this, _, ev: &InputEvent, window, cx| {
                    match ev {
                        InputEvent::PressEnter { .. } => {
                            this.reload_unless_dirty(window, cx, |t| t.page = 0)
                        }
                        // The WHERE preview follows the typing.
                        InputEvent::Change => cx.notify(),
                        _ => {}
                    }
                },
            ));
        }
        // Another column: offer its type's operators, keeping the chosen
        // one when it still fits.
        let op_state = op.clone();
        self._subs.push(cx.subscribe_in(
            &column,
            window,
            move |this, _, ev: &SelectEvent<SearchList>, window, cx| {
                let SelectEvent::Confirm(Some(name)) = ev else {
                    return;
                };
                let ops = this.ops_for(name);
                let current = op_state.read(cx).selected_value().copied();
                let keep = current
                    .and_then(|label| ops.iter().position(|o| *o == label))
                    .unwrap_or(0);
                op_state.update(cx, |state, cx| {
                    state.set_items(ops, window, cx);
                    state.set_selected_index(Some(IndexPath::new(keep)), window, cx);
                });
                cx.notify();
            },
        ));
        self._subs.push(cx.observe(&op, |_, _, cx| cx.notify()));
        self.filters.push(FilterRow {
            column,
            op,
            value,
            value2,
        });
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
        let rx = cx.prompt_for_paths(PathPromptOptions {
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
        if let crate::db::safety::Gate::Block(why) = crate::db::safety::gate_write(&self.shared.cfg)
        {
            return fail(why, window, cx);
        }
        // Read and parse off the UI thread: a big file mustn't freeze it.
        let path = path.to_path_buf();
        let file_name = path
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_default();
        let task = runtime::spawn(async move {
            let bytes = read(&path)
                .await
                .with_context(|| format!("reading {}", path.display()))?;
            let size = bytes.len() as u64;
            let csv = spawn_blocking(move || crate::csv_import::parse(&bytes)).await??;
            Ok((csv, size))
        });
        cx.spawn_in(window, async move |this, cx| {
            let res = task.await;
            this.update_in(cx, |t, window, cx| match res {
                Ok((csv, size)) => t.confirm_import(csv, file_name.clone(), size, window, cx),
                Err(e) => window.push_notification(
                    Notification::error(format!("{e:#}")).title("Import failed"),
                    cx,
                ),
            })
            .ok();
        })
        .detach();
    }

    /// The import dialog (design.md screen 13); Import runs through the
    /// safe-mode gate like any write.
    fn confirm_import(
        &mut self,
        csv: crate::csv_import::Csv,
        file_name: String,
        file_size: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(columns) = self.structure.as_ref().map(|s| s.columns.clone()) else {
            window.push_notification(
                Notification::error("The table's columns haven't loaded yet")
                    .title("Import failed"),
                cx,
            );
            return;
        };
        let atomic = self
            .shared
            .conn
            .try_read()
            .map(|c| c.commits_atomically())
            .unwrap_or(true);
        let me = cx.entity().downgrade();
        let cfg = self.shared.cfg.clone();
        let table = self.object.title();
        let insert_columns = columns.clone();
        let on_import: crate::ui::csv_import_view::OnImport =
            Rc::new(move |changes, window, cx| {
                let rows = changes.len();
                let detail = format!("INSERT INTO {table} … ({rows} rows from the CSV file)");
                let gate = crate::db::safety::gate_write(&cfg);
                let (me_later, columns_later, changes_later) =
                    (me.clone(), insert_columns.clone(), changes.clone());
                if super::guarded(
                    gate,
                    &cfg,
                    "Import",
                    detail,
                    window,
                    cx,
                    move |window, cx| {
                        let (columns, changes) = (columns_later.clone(), changes_later.clone());
                        me_later
                            .update(cx, |t, cx| {
                                t.run_import(changes, columns, atomic, window, cx)
                            })
                            .ok();
                    },
                ) {
                    let columns = insert_columns.clone();
                    me.update(cx, |t, cx| {
                        t.run_import(changes, columns, atomic, window, cx)
                    })
                    .ok();
                }
            });
        let view = crate::ui::csv_import_view::CsvImportView::open(
            csv,
            file_name,
            file_size,
            self.object.title(),
            columns,
            on_import,
            window,
            cx,
        );
        self.import_view = Some(view);
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
        let task =
            runtime::spawn(async move { conn.read().await.commit(&obj, &columns, &changes).await });
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
        if self.committing {
            return;
        }
        let changes = self.table.read(cx).delegate().changes();
        // Writes need the column types (and keys) from the structure.
        let Some(columns) = self.structure.as_ref().map(|s| s.columns.clone()) else {
            window.push_notification(
                Notification::warning(
                    "The table's structure isn't loaded; refresh (⌘R) and try again",
                ),
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
                        window
                            .push_notification(Notification::error(msg).title("Commit failed"), cx)
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
        let t = tokens(cx);
        let mono = cx.theme().mono_font_family.clone();
        // What these conditions become, written by the driver itself.
        let preview = self
            .shared
            .conn
            .try_read()
            .ok()
            .and_then(|conn| conn.filter_preview(&self.current_query(cx)));
        let me = cx.entity();
        v_flex()
            .gap(dp(8.))
            .px(dp(12.))
            .py(dp(10.))
            .bg(t.sidebar)
            .border_b_1()
            .border_color(t.border)
            .on_key_down(cx.listener(|this, ev: &KeyDownEvent, _, cx| {
                if ev.keystroke.key == "escape" {
                    cx.stop_propagation();
                    this.show_filters = false;
                    cx.notify();
                }
            }))
            .when(self.filters.len() > 1, |editor| {
                editor.child(
                    h_flex()
                        .gap(dp(8.))
                        .text_size(dp(12.))
                        .text_color(t.text_secondary)
                        .child("Rows matching")
                        .child(
                            dialog::segmented(
                                "filter-match",
                                &["all", "any"],
                                self.match_any as usize,
                                Rc::new(move |index, window, cx| {
                                    me.update(cx, |this, cx| {
                                        let any = index == 1;
                                        this.reload_unless_dirty(window, cx, |t| {
                                            t.match_any = any;
                                            t.page = 0;
                                        });
                                    })
                                }),
                                cx,
                            )
                            .w(dp(110.)),
                        )
                        .child("of these conditions"),
                )
            })
            .children(self.filters.iter().enumerate().map(|(i, f)| {
                let op =
                    f.op.read(cx)
                        .selected_value()
                        .and_then(|label| FilterOp::ALL.into_iter().find(|o| o.label() == *label));
                let needs_value = op.is_none_or(|op| op.needs_value());
                let between = op == Some(FilterOp::Between);
                let list = matches!(op, Some(FilterOp::In | FilterOp::NotIn));
                h_flex()
                    .gap(dp(6.))
                    .child(
                        div()
                            .w(dp(200.))
                            .child(Select::new(&f.column).cursor_pointer().small()),
                    )
                    .child(
                        div()
                            .w(dp(150.))
                            .child(Select::new(&f.op).cursor_pointer().small()),
                    )
                    .when(needs_value, |row| {
                        row.child(
                            div()
                                .flex_1()
                                .child(Input::new(&f.value).small().cleanable(true)),
                        )
                    })
                    .when(between, |row| {
                        row.child(
                            div()
                                .text_size(dp(12.))
                                .text_color(t.text_muted)
                                .child("and"),
                        )
                        .child(div().flex_1().child(Input::new(&f.value2).small()))
                    })
                    .when(list, |row| {
                        row.child(
                            div()
                                .text_size(dp(11.))
                                .text_color(t.text_faint)
                                .child("comma separated"),
                        )
                    })
                    .when(!needs_value, |row| row.child(div().flex_1()))
                    .child(
                        Button::new(("rm-filter", i))
                            .cursor_pointer()
                            .icon(IconName::X)
                            .ghost()
                            .small()
                            .tooltip("Remove this condition")
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.remove_filter(i, window, cx);
                            })),
                    )
            }))
            .child(
                h_flex()
                    .gap(dp(8.))
                    .child(
                        Button::new("add-filter")
                            .cursor_pointer()
                            .icon(IconName::Plus)
                            .label("Add condition")
                            .ghost()
                            .small()
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.add_filter(window, cx);
                                cx.notify();
                            })),
                    )
                    .when_some(preview, |row, sql| {
                        let copy = sql.clone();
                        row.child(
                            div()
                                .debug_selector(|| "where-preview".into())
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .font_family(mono)
                                .text_size(dp(11.5))
                                .text_color(t.text_muted)
                                .child(sql),
                        )
                        .child(
                            Button::new("copy-where")
                                .cursor_pointer()
                                .icon(IconName::Copy)
                                .ghost()
                                .xsmall()
                                .tooltip("Copy the WHERE clause")
                                .on_click(move |_, window, cx| {
                                    cx.write_to_clipboard(ClipboardItem::new_string(copy.clone()));
                                    window.push_notification("Copied", cx);
                                }),
                        )
                    })
                    .child(div().flex_1())
                    .child(
                        div()
                            .text_size(dp(11.))
                            .text_color(t.text_faint)
                            .child("↵ applies · Esc closes"),
                    )
                    .child(
                        Button::new("apply-filter")
                            .cursor_pointer()
                            .label("Apply")
                            .primary()
                            .small()
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.reload_unless_dirty(window, cx, |t| t.page = 0);
                            })),
                    ),
            )
    }

    /// A segmented control item (Data / Structure / DDL).
    fn segment(
        &self,
        id: &'static str,
        label: &'static str,
        active: bool,
        cx: &mut Context<Self>,
        on: impl Fn(&mut Self) + 'static,
    ) -> impl IntoElement {
        let t = tokens(cx);
        div()
            .id(id)
            .h(dp(24.))
            .px(dp(10.))
            .flex()
            .items_center()
            .rounded(dp(5.))
            .cursor_pointer()
            .text_size(dp(12.))
            .font_weight(FontWeight::MEDIUM)
            .map(|d| {
                if active {
                    d.bg(t.hover).text_color(t.text)
                } else {
                    d.text_color(t.text_secondary)
                        .hover(|d| d.text_color(t.text))
                }
            })
            .child(label)
            .on_click(cx.listener(move |this, _, _, cx| {
                on(this);
                cx.notify();
            }))
    }

    /// A filter chip: column, operator (mono), value coloured by type.
    fn chip(
        &self,
        id: (&'static str, usize),
        filter: &Filter,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let t = tokens(cx);
        let mono = cx.theme().mono_font_family.clone();
        // Numbers bare, text quoted; lists item by item; `between a and b`.
        let shown = |text: &str| {
            if text.parse::<f64>().is_ok() {
                text.to_string()
            } else {
                format!("'{text}'")
            }
        };
        let value = match filter.op {
            op if !op.needs_value() => String::new(),
            FilterOp::In | FilterOp::NotIn => format!(
                "({})",
                FilterOp::list(&filter.value)
                    .iter()
                    .map(|item| shown(item))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            FilterOp::Between => {
                format!("{} and {}", shown(&filter.value), shown(&filter.value2))
            }
            _ => shown(&filter.value),
        };
        let value_color = if filter.value.parse::<f64>().is_ok() {
            t.number
        } else {
            t.success
        };
        h_flex()
            .id(id)
            .flex_none()
            .h(dp(26.))
            .pl(dp(10.))
            .pr(dp(4.))
            .gap(dp(6.))
            .rounded(dp(7.))
            .bg(t.accent.opacity(0.10))
            .border_1()
            .border_color(t.accent.opacity(0.35))
            .text_size(dp(12.))
            .child(div().text_color(t.text).child(filter.column.clone()))
            .child(
                div()
                    .font_family(mono.clone())
                    .text_color(t.text_muted)
                    .child(filter.op.label()),
            )
            .when(!value.is_empty(), |d| {
                d.child(
                    div()
                        .max_w(dp(160.))
                        .truncate()
                        .font_family(mono)
                        .text_color(value_color)
                        .child(value),
                )
            })
    }

    fn render_toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let t = tokens(cx);
        let data = !self.show_structure;
        let ddl = self.show_structure && self.struct_section == 3;
        let structure = self.show_structure && !ddl;
        let grid = self.table.read(cx).delegate();
        let shown = grid.base_rows;
        let page = self.page;
        let pages = self
            .total
            .map(|n| (n as usize).div_ceil(self.page_size).max(1));
        let first = page * self.page_size;
        let range = match (shown, self.total) {
            (0, Some(n)) => format!("0 of {n}"),
            (0, None) => "0 rows".to_string(),
            (_, Some(n)) => format!("{}–{} of {n}", first + 1, first + shown),
            (_, None) => format!("{}–{}", first + 1, first + shown),
        };
        let page_size = self.page_size;
        let me = cx.entity().downgrade();
        let mut chips: Vec<AnyElement> = Vec::new();
        if data {
            for (i, f) in self.fixed.iter().enumerate() {
                chips.push(
                    self.chip(("fixed-chip", i), f, cx)
                        .child(
                            Button::new(("clear-fixed", i))
                                .cursor_pointer()
                                .debug_selector(|| "clear-fixed".into())
                                .icon(IconName::X)
                                .ghost()
                                .xsmall()
                                .tooltip("Show all rows")
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.reload_unless_dirty(window, cx, |t| {
                                        t.fixed.clear();
                                        t.page = 0;
                                    });
                                })),
                        )
                        .into_any_element(),
                );
            }
            for (k, (row, f)) in self.applied.iter().enumerate() {
                let row = *row;
                chips.push(
                    self.chip(("filter-chip", k), f, cx)
                        .cursor_pointer()
                        .on_click(cx.listener(|this, _, window, cx| {
                            if !this.show_filters {
                                this.toggle_filter(&ToggleFilter, window, cx);
                            }
                        }))
                        .child(
                            Button::new(("rm-chip", k))
                                .cursor_pointer()
                                .icon(IconName::X)
                                .ghost()
                                .xsmall()
                                .tooltip("Remove this filter")
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    cx.stop_propagation();
                                    this.remove_filter(row, window, cx);
                                })),
                        )
                        .into_any_element(),
                );
            }
        }
        h_flex()
            .h(dp(48.))
            .flex_none()
            .px(dp(12.))
            .gap(dp(8.))
            .bg(t.canvas)
            .border_b_1()
            .border_color(t.border)
            .child(
                h_flex()
                    .flex_none()
                    .p(dp(3.))
                    .gap(dp(2.))
                    .rounded(dp(7.))
                    .bg(t.surface)
                    .border_1()
                    .border_color(t.border)
                    .map(|group| {
                        if self.shared.cfg.driver != Driver::Mongo {
                            return group.child(self.segment("view-data", "Data", data, cx, |t| {
                                t.show_structure = false;
                            }));
                        }
                        let views = [
                            ("view-documents", "Documents", DocView::Documents),
                            ("view-table", "Table", DocView::Table),
                            ("view-json", "JSON", DocView::Json),
                        ];
                        views.into_iter().fold(group, |group, (id, label, view)| {
                            let active = data && self.doc_view == view;
                            group.child(self.segment(id, label, active, cx, move |t| {
                                t.show_structure = false;
                                t.doc_view = view;
                            }))
                        })
                    })
                    .child(
                        self.segment("view-structure", "Structure", structure, cx, |t| {
                            t.show_structure = true;
                            if t.struct_section == 3 {
                                t.struct_section = 0;
                            }
                        }),
                    )
                    .child(self.segment("view-ddl", "DDL", ddl, cx, |t| {
                        t.show_structure = true;
                        t.struct_section = 3;
                    })),
            )
            .child(
                h_flex()
                    .id("chips")
                    .flex_1()
                    .min_w_0()
                    .gap(dp(6.))
                    .overflow_x_scroll()
                    .children(chips)
                    .when(data, |d| {
                        d.child(
                            Button::new("filter")
                                .cursor_pointer()
                                .icon(IconName::Plus)
                                .label("Filter")
                                .ghost()
                                .small()
                                .selected(self.show_filters)
                                .tooltip("Filter (⌘F)")
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.toggle_filter(&ToggleFilter, window, cx)
                                })),
                        )
                    }),
            )
            .when(data, |d| {
                d.child(
                    h_flex()
                        .flex_none()
                        .gap(dp(2.))
                        .child(
                            Button::new("page-size")
                                .cursor_pointer()
                                .label(range)
                                .ghost()
                                .small()
                                .tooltip("Rows per page")
                                .dropdown_menu(move |menu, _, _| {
                                    PAGE_SIZES.iter().fold(menu, |menu, &n| {
                                        let me = me.clone();
                                        menu.item(
                                            PopupMenuItem::new(format!("{n} rows per page"))
                                                .checked(n == page_size)
                                                .on_click(move |_, window, cx| {
                                                    me.update(cx, |this, cx| {
                                                        this.reload_unless_dirty(window, cx, |t| {
                                                            t.page_size = n;
                                                            t.page = 0;
                                                        })
                                                    })
                                                    .ok();
                                                }),
                                        )
                                    })
                                }),
                        )
                        .child(
                            Button::new("prev-page")
                                .cursor_pointer()
                                .icon(IconName::ChevronLeft)
                                .outline()
                                .small()
                                .tooltip("Previous page")
                                .disabled_when(page == 0)
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.reload_unless_dirty(window, cx, |t| {
                                        t.page = t.page.saturating_sub(1)
                                    });
                                })),
                        )
                        .child(
                            Button::new("next-page")
                                .cursor_pointer()
                                .icon(IconName::ChevronRight)
                                .outline()
                                .small()
                                .tooltip("Next page")
                                .disabled_when(pages.is_some_and(|p| page + 1 >= p))
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.reload_unless_dirty(window, cx, |t| t.page += 1);
                                })),
                        ),
                )
            })
            .child(
                Button::new("refresh")
                    .cursor_pointer()
                    .icon(IconName::RefreshCw)
                    .label("Refresh")
                    .outline()
                    .small()
                    .tooltip("Refresh (⌘R)")
                    .on_click(cx.listener(|this, _, window, cx| this.refresh(window, cx))),
            )
            .when(data, |d| {
                d.child(
                    Button::new("add-row")
                        .cursor_pointer()
                        .icon(IconName::Plus)
                        .label("Row")
                        .outline()
                        .small()
                        .tooltip("Add row (⌘I)")
                        .on_click(
                            cx.listener(|this, _, window, cx| this.add_row(&AddRow, window, cx)),
                        ),
                )
                .child(
                    Button::new("export")
                        .cursor_pointer()
                        .label("Export")
                        .outline()
                        .small()
                        .tooltip("Export (⌘⇧E)")
                        .on_click(|_, window, cx| window.dispatch_action(Box::new(ExportData), cx)),
                )
                .child(
                    Button::new("toggle-detail")
                        .cursor_pointer()
                        .icon(IconName::PanelRight)
                        .ghost()
                        .small()
                        .selected(self.show_detail)
                        .tooltip("Row details (⌘⇧I)")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.show_detail = !this.show_detail;
                            cx.notify();
                        })),
                )
            })
    }

    /// While edits are staged: what's pending, Preview SQL, Discard, Commit.
    /// It floats in a strip below the grid, so it never covers a row.
    fn render_pending(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (edited, new, deleted) = self.table.read(cx).delegate().change_summary();
        let total = edited + new + deleted;
        if total == 0 || self.show_structure {
            return None;
        }
        let t = tokens(cx);
        let mut parts = vec![];
        if edited > 0 {
            parts.push(format!("{edited} edited"));
        }
        if new > 0 {
            parts.push(format!("{new} new"));
        }
        if deleted > 0 {
            parts.push(format!("{deleted} deleted"));
        }
        Some(
            h_flex()
                .flex_none()
                .h(dp(70.))
                .justify_center()
                .child(
                    h_flex()
                        .h(dp(46.))
                        .pl(dp(16.))
                        .pr(dp(6.))
                        .gap(dp(10.))
                        .rounded(dp(12.))
                        .bg(t.raised)
                        .border_1()
                        .border_color(t.border)
                        .shadow_lg()
                        .text_size(dp(13.))
                        .child(div().size(dp(8.)).rounded_full().bg(t.warning))
                        .child(
                            div()
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(if total == 1 {
                                    "1 pending change".to_string()
                                } else {
                                    format!("{total} pending changes")
                                }),
                        )
                        .child(
                            div()
                                .text_size(dp(12.))
                                .text_color(t.text_muted)
                                .child(parts.join(" · ")),
                        )
                        .child(div().w(dp(8.)))
                        .child(
                            Button::new("preview")
                                .cursor_pointer()
                                .label("Preview SQL")
                                .ghost()
                                .small()
                                .on_click(
                                    cx.listener(|this, _, window, cx| this.preview_sql(window, cx)),
                                ),
                        )
                        .child(
                            Button::new("discard")
                                .cursor_pointer()
                                .label("Discard")
                                .ghost()
                                .small()
                                .tooltip("Discard (⌘⇧⌫)")
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.discard(&Discard, window, cx)
                                })),
                        )
                        .child(
                            Button::new("commit")
                                .cursor_pointer()
                                .label("Commit")
                                .primary()
                                .small()
                                .loading(self.committing)
                                .tooltip("Commit (⌘S)")
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.commit(&Commit, window, cx)
                                })),
                        ),
                )
                .into_any_element(),
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

    fn export_total(&self, _cx: &App) -> Option<u64> {
        self.total
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
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.show_structure && self.struct_section == 0 {
            self.ensure_columns_editor(window, cx);
        }
        let theme = cx.theme();
        let body = if self.show_structure {
            let section = self.struct_section;
            let ddl = self
                .structure
                .as_ref()
                .map(|s| s.ddl.clone())
                .unwrap_or_default();
            let content = match section {
                _ if self.structure.is_none() && self.load_started.is_some() => {
                    loading::waiting("Reading the table's structure…", self.load_started, cx)
                }
                0 => match &self.columns_editor {
                    Some(editor) => editor.clone().into_any_element(),
                    None => data_table(&self.struct_cols, cx).into_any_element(),
                },
                1 if self.structure_editable() => self.render_indexes(cx),
                1 => data_table(&self.struct_idx, cx).into_any_element(),
                2 if self.structure_editable() => self.render_foreign_keys(cx),
                2 => data_table(&self.struct_fks, cx).into_any_element(),
                _ => v_flex()
                    .size_full()
                    .child(
                        h_flex()
                            .gap_1()
                            .p_1()
                            .child(
                                Button::new("copy-ddl")
                                    .cursor_pointer()
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
                                    .cursor_pointer()
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
                .when(section < 3, |d| {
                    d.child(
                        TabBar::new("structure-sections")
                            .underline()
                            .small()
                            .selected_index(section)
                            .child(Tab::new().cursor_pointer().label("Columns"))
                            .child(
                                Tab::new()
                                    .cursor_pointer()
                                    .label(format!("Indexes ({idx_count})")),
                            )
                            .child(
                                Tab::new()
                                    .cursor_pointer()
                                    .label(format!("Foreign Keys ({fk_count})")),
                            )
                            .on_click(cx.listener(|this, ix: &usize, _, cx| {
                                this.struct_section = *ix;
                                cx.notify();
                            })),
                    )
                })
                .children(
                    self.structure
                        .iter()
                        .flat_map(|s| s.warnings.clone())
                        .map(|w| {
                            div()
                                .px_2()
                                .py_1()
                                .text_xs()
                                .bg(cx.theme().warning.opacity(0.15))
                                .child(format!("Couldn't load {w}"))
                        }),
                )
                .child(div().flex_1().min_h_0().child(content))
                .into_any_element()
        } else {
            let grid = cell_edit::editable(data_table(&self.table, cx), cx);
            let mongo = self.shared.cfg.driver == Driver::Mongo;
            if mongo && self.doc_view == DocView::Documents {
                render_documents(&self.table.clone(), cx)
            } else if mongo && self.doc_view == DocView::Json {
                let text = page_json(self.table.read(cx).delegate());
                let stale = self
                    .json_view
                    .as_ref()
                    .is_none_or(|(_, shown)| *shown != text);
                if stale {
                    self.json_view = Some((json_editor(text.clone(), window, cx), text));
                }
                let (editor, _) = self.json_view.as_ref().expect("just made");
                render_json(editor)
            } else if let Some(key_view) = self.redis_key.clone() {
                let t = tokens(cx);
                h_flex()
                    .size_full()
                    .child(div().flex_1().min_w_0().h_full().child(grid))
                    .child(
                        div()
                            .w(dp(460.))
                            .flex_none()
                            .h_full()
                            .border_l_1()
                            .border_color(t.border)
                            .child(key_view),
                    )
                    .into_any_element()
            } else if self.show_detail {
                // Tables referencing the shown row, for the References tab.
                let inbound: Vec<InboundLink> = match self.selected_row(cx) {
                    Some(row) => {
                        let tab = cx.entity().downgrade();
                        self.inbound_labels()
                            .into_iter()
                            .enumerate()
                            .map(|(index, label)| {
                                let tab = tab.clone();
                                InboundLink {
                                    label,
                                    open: Rc::new(move |_, cx| {
                                        tab.update(cx, |tab, cx| {
                                            tab.open_referencing(row, index, cx)
                                        })
                                        .ok();
                                    }),
                                }
                            })
                            .collect()
                    }
                    None => vec![],
                };
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
                    .child(row_panel::render(self, &types, &inbound, cx))
                    .into_any_element()
            } else {
                grid.into_any_element()
            }
        };
        let t = tokens(cx);
        let pending = self.render_pending(cx);
        v_flex()
            .size_full()
            .bg(t.canvas)
            .key_context("TableTab")
            .track_focus(&self.focus)
            .on_action(cx.listener(|this, _: &CopyCell, window, cx| this.copy_cell(window, cx)))
            .on_action(cx.listener(|this, _: &SetNull, window, cx| this.set_null(window, cx)))
            .child(self.render_toolbar(cx))
            .when(!self.show_structure, |d| {
                d.children(self.render_find_bar(cx))
            })
            .when(self.show_filters && !self.show_structure, |d| {
                d.child(self.render_filters(cx))
            })
            .when_some(self.error.clone(), |d, e| {
                d.child(
                    h_flex()
                        .gap(dp(8.))
                        .px(dp(12.))
                        .py(dp(8.))
                        .bg(t.danger.opacity(0.10))
                        .border_b_1()
                        .border_color(t.danger.opacity(0.3))
                        .text_size(dp(12.))
                        .text_color(t.danger)
                        .child(Icon::new(IconName::CircleX).small())
                        .child(div().flex_1().child(e)),
                )
            })
            .child(div().flex_1().min_h_0().relative().child(body).when_some(
                self.load_started.filter(|s| loading::visible(Some(*s))),
                |d, since| {
                    d.child(loading::chip(
                        format!("Loading {}", self.object.name),
                        since,
                        Some(
                            Button::new("stop-load")
                                .cursor_pointer()
                                .label("Stop")
                                .on_click(cx.listener(|t, _, _, cx| t.stop_load(cx))),
                        ),
                        cx,
                    ))
                },
            ))
            .children(pending)
    }
}
