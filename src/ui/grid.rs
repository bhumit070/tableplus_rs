//! The spreadsheet grid. Holds the fetched page plus staged edits, which are
//! drawn on top (amber = edited, green = new row, red = deleted) until the
//! user commits or discards them. Cells render by column type (design.md,
//! Cell editors: display rules).

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

use gpui::{
    AnyElement, App, Context, Entity, FontWeight, Hsla, IntoElement, ParentElement, Styled, Window,
    div, prelude::*, px,
};
use gpui_component::input::Input;
use gpui_component::menu::PopupMenu;
use gpui_component::notification::Notification;
use gpui_component::table::{Column, ColumnSort, DataTable, TableDelegate, TableState};
use gpui_component::{ActiveTheme as _, Icon, Sizable as _, Size, WindowExt as _, h_flex, v_flex};
use gpui_kit_assets::IconName;

use super::cell_edit::CellEditor;
use super::theme::{Tokens, dp, enum_color, tokens};
use crate::db::{Change, ColumnInfo, QueryResult};

/// A data table with the design's 32px rows and 40px two-line header,
/// scaled with the interface size.
pub fn data_table<D: TableDelegate>(
    state: &Entity<TableState<D>>,
    cx: &App,
) -> impl IntoElement + use<D> {
    let scale = crate::settings::get(cx).ui_font_size / 13.;
    div().size_full().text_size(dp(12.)).child(
        DataTable::new(state)
            .stripe(true)
            .with_size(Size::Size(px((32. * scale).round())))
            .header_row_height(px((40. * scale).round())),
    )
}

/// How a column's values look (design.md, Cell editors: display rules).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Text,
    Number,
    Bool,
    Json,
    Enum,
}

/// The kind of a column from its database type; `None` when the type isn't
/// known (query results), so the values decide.
fn kind_of_type(ty: &str) -> Option<Kind> {
    let t = ty.trim().to_ascii_lowercase();
    if t.is_empty() {
        return None;
    }
    let base = t.split(['(', ' ']).next().unwrap_or_default();
    Some(match base {
        "bool" | "boolean" => Kind::Bool,
        "tinyint" | "bit" if t.ends_with("(1)") => Kind::Bool,
        "json" | "jsonb" => Kind::Json,
        "int" | "int2" | "int4" | "int8" | "integer" | "smallint" | "bigint" | "tinyint"
        | "mediumint" | "serial" | "bigserial" | "smallserial" | "numeric" | "decimal" | "real"
        | "float" | "float4" | "float8" | "double" | "money" | "number" => Kind::Number,
        _ if base.starts_with("int") || base.ends_with("int") => Kind::Number,
        _ => Kind::Text,
    })
}

fn is_number(v: &str) -> bool {
    let v = v.trim();
    !v.is_empty() && v.len() < 32 && v.parse::<f64>().is_ok_and(f64::is_finite)
}

/// "1204.99" → "1,204.99"; anything that isn't a plain decimal is unchanged.
fn group_thousands(v: &str) -> String {
    let (sign, rest) = match v.strip_prefix('-') {
        Some(r) => ("-", r),
        None => ("", v),
    };
    let (int, frac) = match rest.split_once('.') {
        Some((i, f)) => (i, Some(f)),
        None => (rest, None),
    };
    if int.is_empty() || !int.bytes().all(|b| b.is_ascii_digit()) || int.len() <= 3 {
        return v.to_string();
    }
    let mut out = String::new();
    for (i, ch) in int.chars().enumerate() {
        if i > 0 && (int.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    match frac {
        Some(f) => format!("{sign}{out}.{f}"),
        None => format!("{sign}{out}"),
    }
}

fn truthy(v: &str) -> Option<bool> {
    match v.trim().to_ascii_lowercase().as_str() {
        "t" | "true" | "1" | "yes" | "y" => Some(true),
        "f" | "false" | "0" | "no" | "n" => Some(false),
        _ => None,
    }
}

/// A NULL / DEFAULT marker: never an empty-looking cell.
fn marker(text: &'static str, fg: Hsla, bg: Hsla) -> gpui::Div {
    div()
        .flex_none()
        .px(dp(5.))
        .h(dp(17.))
        .flex()
        .items_center()
        .rounded(dp(4.))
        .bg(bg)
        .text_color(fg)
        .text_size(dp(10.5))
        .font_weight(FontWeight::MEDIUM)
        .child(text)
}

/// The pill an enum value is shown as: a coloured dot and the value.
pub fn enum_pill(value: &str, cx: &App) -> gpui::Div {
    let color = enum_color(value, cx);
    h_flex()
        .flex_none()
        .min_w_0()
        .h(dp(19.))
        .px(dp(7.))
        .gap(dp(5.))
        .rounded(dp(9.))
        .bg(color.opacity(0.15))
        .text_color(color)
        .text_size(dp(11.5))
        .child(div().flex_none().size(dp(6.)).rounded_full().bg(color))
        .child(div().truncate().child(value.to_string()))
}

/// A checkbox with the literal next to it, so true / false / NULL are never
/// confused.
fn bool_cell(v: bool, raw: &str, t: &Tokens) -> gpui::Div {
    h_flex()
        .gap(dp(6.))
        .child(
            div()
                .flex_none()
                .size(dp(13.))
                .rounded(dp(3.))
                .flex()
                .items_center()
                .justify_center()
                .when(v, |d| d.bg(t.accent))
                .when(!v, |d| d.border_1().border_color(t.text_faint))
                .when(v, |d| {
                    d.child(
                        Icon::new(IconName::Check)
                            .size(dp(10.))
                            .text_color(t.canvas),
                    )
                }),
        )
        .child(raw.to_string())
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
    /// Per column: the database type ("int8", "numeric(12,2)"), shown under
    /// the name; empty when unknown.
    pub types: Vec<String>,
    /// Per column: the table a foreign key points to, shown as "→ table".
    pub refs: Vec<Option<String>>,
    kinds: Vec<Kind>,
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
        self.types = vec![String::new(); self.columns.len()];
        self.refs = vec![None; self.columns.len()];
        self.links = vec![false; self.columns.len()];
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
                (longest as f32 * 7.4 + 32.).clamp(72., 380.)
            })
            .collect();
        self.update_kinds();
    }

    /// Types and foreign-key targets for the header, from the table's
    /// columns. Call after `set_result` (and `set_choices`).
    pub fn set_types(&mut self, table: &[ColumnInfo], refs: &[(String, String)]) {
        self.types = self
            .columns
            .iter()
            .map(|name| {
                table
                    .iter()
                    .find(|t| &t.name == name)
                    .map(|t| t.data_type.clone())
                    .unwrap_or_default()
            })
            .collect();
        self.refs = self
            .columns
            .iter()
            .map(|name| refs.iter().find(|(c, _)| c == name).map(|(_, t)| t.clone()))
            .collect();
        // The second header line and "→ table" need room too.
        for c in 0..self.columns.len() {
            let sub = self.types[c].chars().count()
                + self.refs[c].as_ref().map_or(0, |r| r.chars().count() + 3);
            let need = (sub as f32 * 6.4 + 40.).min(380.);
            if let Some(w) = self.widths.get_mut(c) {
                *w = w.max(need);
            }
        }
        self.update_kinds();
    }

    /// From the declared type, else from the values (query results).
    fn update_kinds(&mut self) {
        self.kinds = (0..self.columns.len())
            .map(|c| {
                if self.choices.get(c).is_some_and(|ch| !ch.is_empty()) {
                    return Kind::Enum;
                }
                if let Some(k) = self.types.get(c).and_then(|t| kind_of_type(t)) {
                    return k;
                }
                let mut values = self.rows.iter().take(200).filter_map(|r| r[c].as_deref());
                let first = values.next();
                if first.is_some_and(is_number) && values.all(is_number) {
                    Kind::Number
                } else {
                    Kind::Text
                }
            })
            .collect();
    }

    pub fn kind(&self, c: usize) -> Kind {
        self.kinds.get(c).copied().unwrap_or(Kind::Text)
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
        self.update_kinds();
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

    /// (edited rows, new rows, deleted rows), for the pending-changes bar.
    pub fn change_summary(&self) -> (usize, usize, usize) {
        let edited: BTreeSet<usize> = self
            .edits
            .keys()
            .map(|(r, _)| *r)
            .filter(|r| *r < self.base_rows && !self.deleted.contains(r))
            .collect();
        (
            edited.len(),
            self.rows.len() - self.base_rows,
            self.deleted.len(),
        )
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
        // No `.sort(…)`: the header draws its own arrow and sorts on click.
        Column::new(format!("{col_ix}:{name}"), name).width(px(self
            .widths
            .get(col_ix)
            .copied()
            .unwrap_or(120.)))
    }

    /// Two lines: the name (key glyph for primary keys, arrow when sorted),
    /// then the type and "→ table" for foreign keys. Clicking sorts:
    /// ascending, descending, off.
    fn render_th(
        &mut self,
        col_ix: usize,
        _window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let t = tokens(cx);
        let mono = cx.theme().mono_font_family.clone();
        let name = self.columns.get(col_ix).cloned().unwrap_or_default();
        let pk = self.pk.get(col_ix).copied().unwrap_or(false);
        let ty = self.types.get(col_ix).cloned().unwrap_or_default();
        let target = self.refs.get(col_ix).cloned().flatten();
        let sorted = self.sort.filter(|(c, _)| *c == col_ix).map(|(_, asc)| asc);
        let right = self.kind(col_ix) == Kind::Number;
        let next = match sorted {
            None => ColumnSort::Ascending,
            Some(true) => ColumnSort::Descending,
            Some(false) => ColumnSort::Default,
        };
        v_flex()
            .id(("th", col_ix))
            .size_full()
            .min_w_0()
            .justify_center()
            .gap(dp(1.))
            .cursor_pointer()
            .on_click(cx.listener(move |table, _, window, cx| {
                cx.stop_propagation();
                table.delegate_mut().perform_sort(col_ix, next, window, cx);
            }))
            .child(
                h_flex()
                    .w_full()
                    .gap(dp(4.))
                    .when(right, |d| d.justify_end())
                    .when(pk, |d| {
                        d.child(
                            Icon::new(IconName::KeyRound)
                                .size(dp(11.))
                                .text_color(t.warning),
                        )
                    })
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_size(dp(12.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(t.text)
                            .child(name),
                    )
                    .when_some(sorted, |d, asc| {
                        d.child(
                            Icon::new(if asc {
                                IconName::ArrowUp
                            } else {
                                IconName::ArrowDown
                            })
                            .size(dp(12.))
                            .text_color(t.accent),
                        )
                    }),
            )
            .when(!ty.is_empty() || target.is_some(), |d| {
                d.child(
                    h_flex()
                        .w_full()
                        .gap(dp(6.))
                        .when(right, |d| d.justify_end())
                        .font_family(mono)
                        .text_size(dp(10.))
                        .child(div().truncate().text_color(t.text_muted).child(ty))
                        .when_some(target, |d, target| {
                            d.child(
                                div()
                                    .flex_none()
                                    .text_color(t.info)
                                    .child(format!("→ {target}")),
                            )
                        }),
                )
            })
    }

    /// Staged rows: a tint and a 2px bar on the left edge.
    fn render_tr(
        &mut self,
        row_ix: usize,
        _window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> gpui::Stateful<gpui::Div> {
        let t = tokens(cx);
        let state = if self.deleted.contains(&row_ix) {
            Some(t.danger)
        } else if row_ix >= self.base_rows {
            Some(t.success)
        } else {
            None
        };
        div()
            .id(("row", row_ix))
            .relative()
            .when_some(state, |d, color| {
                d.bg(color.opacity(0.10)).child(
                    div()
                        .absolute()
                        .left_0()
                        .top_0()
                        .bottom_0()
                        .w(px(2.))
                        .bg(color),
                )
            })
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
        let t = tokens(cx);
        let mono = cx.theme().mono_font_family.clone();
        let new_row = row_ix >= self.base_rows;
        let deleted = self.deleted.contains(&row_ix);
        let edited = self.edits.contains_key(&(row_ix, col_ix));
        let kind = self.kind(col_ix);
        let link = self.links.get(col_ix).copied().unwrap_or(false) && !new_row;
        let pk = self.pk.get(col_ix).copied().unwrap_or(false);
        let right = kind == Kind::Number;
        let cell = h_flex()
            .size_full()
            .min_w_0()
            .gap(dp(6.))
            .px(dp(10.))
            .overflow_hidden()
            .font_family(mono)
            .text_size(dp(12.))
            .text_color(if deleted { t.text_muted } else { t.text })
            .when(right, |d| d.justify_end())
            .when(edited && !new_row, |d| {
                d.bg(t.warning.opacity(0.14))
                    .border_1()
                    .border_color(t.warning)
            })
            .when(deleted, |d| d.line_through());
        let value = self.value(row_ix, col_ix).cloned().flatten();
        let content: AnyElement = match value.as_deref() {
            None if new_row => {
                marker("DEFAULT", t.keyword, t.keyword.opacity(0.14)).into_any_element()
            }
            None => marker("NULL", t.text_muted, t.hover).into_any_element(),
            Some("") => div()
                .text_color(t.text_faint)
                .child("''")
                .into_any_element(),
            Some(v) => match kind {
                Kind::Enum => enum_pill(v, cx).into_any_element(),
                Kind::Bool => match truthy(v) {
                    Some(b) => bool_cell(b, v, &t).into_any_element(),
                    None => div().truncate().child(display(v)).into_any_element(),
                },
                Kind::Number => {
                    let negative = v.trim_start().starts_with('-');
                    let shown = if pk || link {
                        v.to_string()
                    } else {
                        group_thousands(v)
                    };
                    div()
                        .truncate()
                        .when(negative && !link, |d| d.text_color(t.danger))
                        .when(link, |d| d.text_color(t.info))
                        .child(shown)
                        .into_any_element()
                }
                _ => div()
                    .min_w_0()
                    .truncate()
                    .when(link, |d| d.text_color(t.info))
                    .child(display(v))
                    .into_any_element(),
            },
        };
        let cell = cell.child(content);
        if link && value.is_some() {
            let follow = self.on_follow.clone();
            let id = row_ix * self.columns.len().max(1) + col_ix;
            return cell
                .child(
                    div()
                        .id(("fk-link", id))
                        .debug_selector(move || format!("fk-link-{row_ix}-{col_ix}"))
                        .flex_none()
                        .cursor_pointer()
                        .text_color(t.info.opacity(0.7))
                        .hover(|d| d.text_color(t.info))
                        .child(Icon::new(IconName::ArrowRight).size(dp(12.)))
                        .on_click(move |_, window, cx| {
                            cx.stop_propagation();
                            if let Some(f) = &follow {
                                f(row_ix, col_ix, window, cx);
                            }
                        }),
                )
                .into_any_element();
        }
        cell.into_any_element()
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
            .text_color(tokens(cx).text_muted)
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
