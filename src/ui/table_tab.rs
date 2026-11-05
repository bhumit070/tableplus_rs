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
use gpui_component::table::{TableEvent, TableState};
use gpui_component::{
    ActiveTheme as _, Disableable as _, IndexPath, Selectable as _, Sizable as _, WindowExt as _,
    h_flex, v_flex,
};
use gpui_kit_assets::IconName;

use super::cell_edit::{self, EditStart};
use super::grid::{Grid, data_table};
use super::{
    AddRow, Commit, CopyCell, DeleteRows, Discard, SearchList, SetNull, Shared, ToggleFilter,
    search_list,
};
use crate::db::{DbObject, EnvTag, Filter, FilterOp, QueryResult, Structure, TableQuery};
use crate::runtime;

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
    show_structure: bool,
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
            .item(item("Copy Row as JSON", |t, row, window, cx| {
                let json = t.table.read(cx).delegate().row_json(row);
                cx.write_to_clipboard(ClipboardItem::new_string(json));
                window.push_notification("Row copied as JSON", cx);
            }))
            .item(item("Copy Page as CSV", |t, _, window, cx| {
                let csv = t.table.read(cx).delegate().to_csv();
                cx.write_to_clipboard(ClipboardItem::new_string(csv));
                window.push_notification("Page copied as CSV", cx);
            }))
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

        let subs = vec![
            cx.subscribe_in(&table, window, |this, _, ev: &TableEvent, window, cx| {
                if let TableEvent::DoubleClickedCell(r, c) = ev {
                    cell_edit::start_edit(this, *r, *c, EditStart::Keep, window, cx);
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
            show_structure: false,
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

    pub fn reload(&mut self, cx: &mut Context<Self>) {
        let grid = self.table.read(cx).delegate();
        let order = grid
            .sort
            .and_then(|(c, asc)| Some((grid.columns.get(c)?.clone(), asc)));
        let q = TableQuery {
            object: self.object.clone(),
            filters: if self.show_filters {
                self.filter_values(cx)
            } else {
                vec![]
            },
            order,
            limit: self.page_size,
            offset: self.page * self.page_size,
        };
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
        self.table.update(cx, |t, cx| {
            let g = t.delegate_mut();
            g.set_result(&data);
            g.pk = g.columns.iter().map(|c| pk_names.contains(c)).collect();
            g.editable = vec![true; g.columns.len()];
            t.refresh(cx);
            cx.notify();
        });
        cx.notify();
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
            (None, Some(r)) => t.delegate().row_json(r),
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
        if self.shared.cfg.tag == EnvTag::Production {
            let preview = self
                .shared
                .conn
                .try_read()
                .map(|c| c.preview(&self.object, &columns, &changes))
                .unwrap_or_default();
            let weak = cx.entity().downgrade();
            window.open_alert_dialog(cx, move |d, _, _| {
                let weak = weak.clone();
                d.title("Commit to PRODUCTION?")
                    .description(SharedString::from(preview.clone()))
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
        let changes = self.table.read(cx).delegate().changes();
        let columns = self
            .structure
            .as_ref()
            .map(|s| s.columns.clone())
            .unwrap_or_default();
        let conn = self.shared.conn.clone();
        let obj = self.object.clone();
        let n = changes.len();
        let task =
            runtime::spawn(async move { conn.read().await.commit(&obj, &columns, &changes).await });
        cx.spawn_in(window, async move |this, cx| {
            let res = task.await;
            this.update_in(cx, |this, window, cx| match res {
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
            v_flex()
                .size_full()
                .child(
                    div()
                        .flex_1()
                        .min_h_0()
                        .child(data_table(&self.struct_cols, cx)),
                )
                .child(
                    div()
                        .px_2()
                        .py_1()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .border_t_1()
                        .border_color(theme.border)
                        .child("INDEXES"),
                )
                .child(div().h(rems(11.25)).child(data_table(&self.struct_idx, cx)))
                .into_any_element()
        } else {
            cell_edit::editable(data_table(&self.table, cx), cx).into_any_element()
        };
        v_flex()
            .size_full()
            .key_context("TableTab")
            .track_focus(&self.focus)
            .on_action(cx.listener(|this, _: &CopyCell, window, cx| this.copy_cell(window, cx)))
            .on_action(cx.listener(|this, _: &SetNull, window, cx| this.set_null(window, cx)))
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
