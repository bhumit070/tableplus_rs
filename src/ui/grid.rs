//! The spreadsheet grid. Holds the fetched page plus staged edits, which are
//! drawn on top (yellow = edited, green = new row, red = deleted) until the
//! user commits or discards them.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

use gpui::{App, Context, Entity, IntoElement, ParentElement, Styled, Window, div, prelude::*, px};
use gpui_component::Size;
use gpui_component::input::Input;
use gpui_component::menu::PopupMenu;
use gpui_component::notification::Notification;
use gpui_component::table::{Column, ColumnSort, DataTable, TableDelegate, TableState};
use gpui_component::{ActiveTheme as _, Sizable as _, WindowExt as _};

use super::cell_edit::CellEditor;
use crate::db::{Change, ColumnInfo, QueryResult};

/// A data table whose row height follows the interface font size
/// (30px rows at the default 16px, like the library's "small" size).
pub fn data_table<D: TableDelegate>(
    state: &Entity<TableState<D>>,
    cx: &App,
) -> impl IntoElement + use<D> {
    let row = (crate::settings::get(cx).ui_font_size * 1.875).round();
    div().size_full().text_sm().child(
        DataTable::new(state)
            .stripe(true)
            .with_size(Size::Size(px(row))),
    )
}

pub type SortFn = Rc<dyn Fn(usize, ColumnSort, &mut Window, &mut App)>;
pub type MenuFn = Rc<dyn Fn(usize, PopupMenu, &mut Window, &mut App) -> PopupMenu>;
/// Called with (row, column) when a foreign-key link is clicked.
pub type FollowFn = Rc<dyn Fn(usize, usize, &mut Window, &mut App)>;

#[derive(Default)]
pub struct Grid {
    pub columns: Vec<String>,
    pub pk: Vec<bool>,
    pub rows: Vec<Vec<Option<String>>>,
    widths: Vec<f32>,
    /// Rows at index >= `base_rows` were added locally.
    pub base_rows: usize,
    pub edits: BTreeMap<(usize, usize), Option<String>>,
    pub deleted: BTreeSet<usize>,
    pub editing: Option<(usize, usize, CellEditor)>,
    /// Which columns can be edited; empty = read-only grid.
    pub editable: Vec<bool>,
    pub loading: bool,
    pub sort: Option<(usize, bool)>,
    /// Server-side sort; when unset the grid sorts the loaded rows itself.
    pub on_sort: Option<SortFn>,
    pub on_menu: Option<MenuFn>,
    /// Per column: the values an enum column allows (`None` = NULL), edited
    /// with a dropdown; empty = free text.
    pub choices: Vec<Vec<Option<String>>>,
    /// Columns that reference another table: their values get a → link.
    pub links: Vec<bool>,
    pub on_follow: Option<FollowFn>,
}

fn display(v: &str) -> String {
    let mut s: String = v.chars().take(400).collect();
    if s.contains(['\n', '\r', '\t']) {
        s = s
            .replace("\r\n", "↵")
            .replace(['\n', '\r'], "↵")
            .replace('\t', " ");
    }
    s
}

fn cmp_values(a: &Option<String>, b: &Option<String>) -> Ordering {
    match (a, b) {
        (None, None) => Ordering::Equal,
        (None, _) => Ordering::Less,
        (_, None) => Ordering::Greater,
        (Some(a), Some(b)) => match (a.parse::<f64>(), b.parse::<f64>()) {
            (Ok(x), Ok(y)) => x.partial_cmp(&y).unwrap_or(Ordering::Equal),
            _ => a.cmp(b),
        },
    }
}

impl Grid {
    pub fn set_result(&mut self, res: &QueryResult) {
        self.columns = res.columns.clone();
        self.rows = res.rows.clone();
        self.base_rows = self.rows.len();
        // Flags from the previous result don't describe these columns.
        self.pk = vec![false; self.columns.len()];
        self.choices = vec![vec![]; self.columns.len()];
        self.edits.clear();
        self.deleted.clear();
        self.editing = None;
        self.widths = (0..self.columns.len())
            .map(|c| {
                let longest = self
                    .rows
                    .iter()
                    .take(100)
                    .map(|r| r[c].as_deref().map_or(4, |s| s.chars().take(60).count()))
                    .max()
                    .unwrap_or(0)
                    .max(self.columns[c].chars().count() + 2);
                (longest as f32 * 7.2 + 24.).clamp(64., 380.)
            })
            .collect();
    }

    /// Dropdown values for the result columns that are enum columns of
    /// `table` (NULL last when the column allows it).
    pub fn set_choices(&mut self, table: &[ColumnInfo]) {
        self.choices = self
            .columns
            .iter()
            .map(|name| match table.iter().find(|t| &t.name == name) {
                Some(t) if !t.enum_values.is_empty() => t
                    .enum_values
                    .iter()
                    .cloned()
                    .map(Some)
                    .chain(t.nullable.then_some(None))
                    .collect(),
                _ => vec![],
            })
            .collect();
    }

    /// Not while a page loads: its arrival would drop the edit unsaved.
    pub fn can_edit(&self, r: usize, c: usize) -> bool {
        !self.loading
            && self.editable.get(c).copied().unwrap_or(false)
            && r < self.rows.len()
            && !self.deleted.contains(&r)
    }

    pub fn value(&self, r: usize, c: usize) -> Option<&Option<String>> {
        self.edits.get(&(r, c)).or_else(|| self.rows.get(r)?.get(c))
    }

    pub fn original(&self, r: usize, c: usize) -> Option<&Option<String>> {
        self.rows.get(r)?.get(c)
    }

    pub fn change_count(&self) -> usize {
        let edited_rows: BTreeSet<usize> = self
            .edits
            .keys()
            .map(|(r, _)| *r)
            .filter(|r| *r < self.base_rows && !self.deleted.contains(r))
            .collect();
        edited_rows.len() + self.deleted.len() + (self.rows.len() - self.base_rows)
    }

    pub fn is_dirty(&self) -> bool {
        self.change_count() > 0
    }

    pub fn set_cell(&mut self, r: usize, c: usize, v: Option<String>) {
        if r >= self.rows.len() || self.loading {
            return;
        }
        if self.original(r, c) == Some(&v) && r < self.base_rows {
            self.edits.remove(&(r, c));
        } else {
            self.edits.insert((r, c), v);
        }
    }

    pub fn add_row(&mut self) -> usize {
        self.rows.push(vec![None; self.columns.len()]);
        self.rows.len() - 1
    }

    pub fn toggle_delete(&mut self, r: usize) {
        if self.loading {
            return;
        }
        if r >= self.base_rows {
            // New rows are simply dropped.
            if r < self.rows.len() {
                self.rows.remove(r);
                self.edits = std::mem::take(&mut self.edits)
                    .into_iter()
                    .filter(|((er, _), _)| *er != r)
                    .map(|((er, c), v)| ((if er > r { er - 1 } else { er }, c), v))
                    .collect();
            }
        } else if !self.deleted.remove(&r) {
            self.deleted.insert(r);
        }
    }

    pub fn discard(&mut self) {
        self.rows.truncate(self.base_rows);
        self.edits.clear();
        self.deleted.clear();
        self.editing = None;
    }

    /// Converts staged edits into backend changes. Rows are identified by
    /// primary-key columns, or by every column when there is no key.
    pub fn changes(&self) -> Vec<Change> {
        let has_pk = self.pk.iter().any(|p| *p);
        let key = |r: usize| -> Vec<(String, Option<String>)> {
            self.columns
                .iter()
                .enumerate()
                .filter(|(c, _)| !has_pk || self.pk[*c])
                .map(|(c, name)| (name.clone(), self.rows[r][c].clone()))
                .collect()
        };
        let mut out = Vec::new();
        let mut by_row: BTreeMap<usize, Vec<(String, Option<String>)>> = BTreeMap::new();
        for ((r, c), v) in &self.edits {
            if *r < self.base_rows && !self.deleted.contains(r) {
                by_row
                    .entry(*r)
                    .or_default()
                    .push((self.columns[*c].clone(), v.clone()));
            }
        }
        for (r, set) in by_row {
            out.push(Change::Update { key: key(r), set });
        }
        for r in self.base_rows..self.rows.len() {
            let values = (0..self.columns.len())
                .map(|c| (self.columns[c].clone(), self.value(r, c).cloned().flatten()))
                .collect();
            out.push(Change::Insert { values });
        }
        for r in &self.deleted {
            out.push(Change::Delete { key: key(*r) });
        }
        out
    }

    pub fn cell_string(&self, r: usize, c: usize) -> Option<String> {
        self.value(r, c).cloned().flatten()
    }

    /// Row `r` as shown (staged edits applied).
    pub fn row_values(&self, r: usize) -> crate::export::Row {
        (0..self.columns.len())
            .map(|c| self.cell_string(r, c))
            .collect()
    }

    /// Every row as shown, minus rows marked for deletion.
    pub fn all_values(&self) -> Vec<crate::export::Row> {
        (0..self.rows.len())
            .filter(|r| !self.deleted.contains(r))
            .map(|r| self.row_values(r))
            .collect()
    }

    pub fn column_values(&self, c: usize) -> Vec<Option<String>> {
        (0..self.rows.len())
            .filter(|r| !self.deleted.contains(r))
            .map(|r| self.cell_string(r, c))
            .collect()
    }
}

impl TableDelegate for Grid {
    fn columns_count(&self, _cx: &App) -> usize {
        self.columns.len()
    }

    fn rows_count(&self, _cx: &App) -> usize {
        self.rows.len()
    }

    fn column(&self, col_ix: usize, _cx: &App) -> Column {
        let name = self.columns.get(col_ix).cloned().unwrap_or_default();
        let label = if self.pk.get(col_ix).copied().unwrap_or(false) {
            format!("🔑 {name}")
        } else {
            name.clone()
        };
        let sort = match self.sort {
            Some((c, true)) if c == col_ix => ColumnSort::Ascending,
            Some((c, false)) if c == col_ix => ColumnSort::Descending,
            _ => ColumnSort::Default,
        };
        Column::new(format!("{col_ix}:{name}"), label)
            .width(px(self.widths.get(col_ix).copied().unwrap_or(120.)))
            .sort(sort)
    }

    fn perform_sort(
        &mut self,
        col_ix: usize,
        sort: ColumnSort,
        window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) {
        // Staged edits point at row positions, and a sort (local or a
        // server reload) moves the rows under them.
        if self.is_dirty() {
            window.push_notification(
                Notification::warning("Commit (⌘S) or discard (⌘⇧⌫) your changes before sorting"),
                cx,
            );
            // Put the header's sort arrow back.
            let table = cx.entity();
            window.defer(cx, move |_, cx| table.update(cx, |t, cx| t.refresh(cx)));
            return;
        }
        self.sort = match sort {
            ColumnSort::Ascending => Some((col_ix, true)),
            ColumnSort::Descending => Some((col_ix, false)),
            ColumnSort::Default => None,
        };
        if let Some(f) = self.on_sort.clone() {
            f(col_ix, sort, window, cx);
            return;
        }
        match self.sort {
            Some((c, asc)) => self.rows.sort_by(|a, b| {
                let o = cmp_values(&a[c], &b[c]);
                if asc { o } else { o.reverse() }
            }),
            None => {}
        }
        cx.notify();
    }

    fn render_td(
        &mut self,
        row_ix: usize,
        col_ix: usize,
        _window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        if let Some((r, c, editor)) = &self.editing {
            if *r == row_ix && *c == col_ix {
                // Borderless and cell-sized, so editing happens in place.
                let cell = div().size_full().key_context("CellEditor");
                return match editor {
                    CellEditor::Text(input) => {
                        cell.child(Input::new(input).appearance(false).h_full())
                    }
                    CellEditor::Choice(choices) => cell.child(choices.clone()),
                }
                .into_any_element();
            }
        }
        let theme = cx.theme();
        let new_row = row_ix >= self.base_rows;
        let deleted = self.deleted.contains(&row_ix);
        let edited = self.edits.contains_key(&(row_ix, col_ix));
        let bg = if deleted {
            Some(theme.danger.opacity(0.18))
        } else if new_row {
            Some(theme.success.opacity(0.16))
        } else if edited {
            Some(theme.warning.opacity(0.25))
        } else {
            None
        };
        let cell = div()
            .size_full()
            .flex()
            .items_center()
            .px_1()
            .overflow_hidden()
            .when_some(bg, |d, bg| d.bg(bg))
            .when(deleted, |d| d.line_through());
        let link = self.links.get(col_ix).copied().unwrap_or(false) && !new_row;
        match self.value(row_ix, col_ix) {
            Some(Some(v)) if link => {
                let follow = self.on_follow.clone();
                let id = row_ix * self.columns.len().max(1) + col_ix;
                cell.gap_1()
                    .child(div().flex_1().min_w_0().truncate().child(display(v)))
                    .child(
                        div()
                            .id(("fk-link", id))
                            .debug_selector(move || format!("fk-link-{row_ix}-{col_ix}"))
                            .flex_none()
                            .cursor_pointer()
                            .text_color(theme.muted_foreground)
                            .hover(|d| d.text_color(theme.primary))
                            .child(
                                gpui_component::Icon::new(gpui_kit_assets::IconName::ArrowRight)
                                    .xsmall(),
                            )
                            .on_click(move |_, window, cx| {
                                cx.stop_propagation();
                                if let Some(f) = &follow {
                                    f(row_ix, col_ix, window, cx);
                                }
                            }),
                    )
                    .into_any_element()
            }
            Some(Some(v)) => cell
                .child(div().truncate().child(display(v)))
                .into_any_element(),
            _ => cell
                .text_color(theme.muted_foreground.opacity(0.7))
                .italic()
                .child(if new_row { "DEFAULT" } else { "NULL" })
                .into_any_element(),
        }
    }

    fn context_menu(
        &mut self,
        row_ix: usize,
        menu: PopupMenu,
        window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> PopupMenu {
        match self.on_menu.clone() {
            Some(f) => f(row_ix, menu, window, cx),
            None => menu,
        }
    }

    fn cell_text(&self, row_ix: usize, col_ix: usize, _cx: &App) -> String {
        self.cell_string(row_ix, col_ix)
            .unwrap_or_else(|| "NULL".into())
    }

    fn loading(&self, _cx: &App) -> bool {
        self.loading
    }

    fn render_empty(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .text_color(cx.theme().muted_foreground)
            .child(if self.columns.is_empty() {
                ""
            } else {
                "No rows"
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grid() -> Grid {
        let mut g = Grid::default();
        g.set_result(&QueryResult {
            columns: vec!["id".into(), "name".into()],
            rows: vec![
                vec![Some("1".into()), Some("a".into())],
                vec![Some("2".into()), None],
            ],
            ..Default::default()
        });
        g.pk = vec![true, false];
        g
    }

    #[test]
    fn edits_inserts_deletes_become_changes() {
        let mut g = grid();
        g.set_cell(0, 1, Some("b".into()));
        g.set_cell(1, 1, None); // unchanged → not staged
        let r = g.add_row();
        g.set_cell(r, 1, Some("new".into()));
        g.toggle_delete(1);
        assert_eq!(g.change_count(), 3);
        let ch = g.changes();
        assert!(
            matches!(&ch[0], Change::Update { key, set } if key == &vec![("id".into(), Some("1".into()))] && set[0].1.as_deref() == Some("b"))
        );
        assert!(
            matches!(&ch[1], Change::Insert { values } if values[1].1.as_deref() == Some("new") && values[0].1.is_none())
        );
        assert!(matches!(&ch[2], Change::Delete { key } if key[0].1.as_deref() == Some("2")));
        g.discard();
        assert!(!g.is_dirty());
        assert_eq!(g.rows.len(), 2);
    }

    #[test]
    fn removing_new_row_shifts_edits() {
        let mut g = grid();
        let a = g.add_row();
        let b = g.add_row();
        g.set_cell(b, 1, Some("keep".into()));
        g.toggle_delete(a);
        assert_eq!(g.rows.len(), 3);
        assert_eq!(g.cell_string(2, 1).as_deref(), Some("keep"));
    }

    #[test]
    fn no_pk_uses_all_columns_as_key() {
        let mut g = grid();
        g.pk = vec![false, false];
        g.toggle_delete(0);
        let ch = g.changes();
        assert!(matches!(&ch[0], Change::Delete { key } if key.len() == 2));
    }
}
