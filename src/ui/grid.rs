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
use super::cell_value::{Temporal, check, date_part, now_for, temporal, with_date};
use super::theme::{Tokens, dp, enum_color, tokens};
use super::{CellDown, CellDownMore, CellUp, CellUpMore};
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
    /// Postgres arrays and MySQL SET: shown as chips.
    Array,
    /// Bytes: shown as their type and size, not as hex.
    Binary,
}

/// The kind of a column from its database type; `None` when the type isn't
/// known (query results), so the values decide.
fn kind_of_type(ty: &str) -> Option<Kind> {
    let t = ty.trim().to_ascii_lowercase();
    if t.is_empty() {
        return None;
    }
    if t.ends_with("[]") {
        return Some(Kind::Array);
    }
    let base = t.split(['(', ' ']).next().unwrap_or_default();
    Some(match base {
        "set" => Kind::Array,
        "bytea" | "blob" | "tinyblob" | "mediumblob" | "longblob" | "binary" | "varbinary" => {
            Kind::Binary
        }
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

/// The items of a Postgres array literal (`{a,"b c",NULL}`) or a MySQL SET
/// value (`a,b`).
pub fn array_items(v: &str) -> Vec<String> {
    let inner = v
        .trim()
        .strip_prefix('{')
        .and_then(|r| r.strip_suffix('}'))
        .unwrap_or(v.trim());
    if inner.is_empty() {
        return vec![];
    }
    let (mut out, mut cur, mut quoted, mut escaped) = (vec![], String::new(), false, false);
    for ch in inner.chars() {
        match ch {
            _ if escaped => {
                cur.push(ch);
                escaped = false;
            }
            '\\' if quoted => escaped = true,
            '"' => quoted = !quoted,
            ',' if !quoted => out.push(std::mem::take(&mut cur)),
            _ => cur.push(ch),
        }
    }
    out.push(cur);
    out
}

/// "\\x0a1b…" (Postgres) or "0x0a1b…" (SQLite) → the number of bytes.
fn hex_len(v: &str) -> Option<usize> {
    let hex = v.strip_prefix("\\x").or_else(|| v.strip_prefix("0x"))?;
    (hex.len() % 2 == 0 && hex.bytes().all(|b| b.is_ascii_hexdigit())).then_some(hex.len() / 2)
}

/// 24100 → "23.5 KB".
fn human_size(n: usize) -> String {
    match n {
        0..1024 => format!("{n} B"),
        1024..1_048_576 => format!("{:.1} KB", n as f64 / 1024.),
        _ => format!("{:.1} MB", n as f64 / 1_048_576.),
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
    // Natural width: enum columns are sized to their longest value.
    h_flex()
        .flex_none()
        .h(dp(19.))
        .px(dp(7.))
        .gap(dp(5.))
        .rounded(dp(9.))
        .bg(color.opacity(0.15))
        .text_color(color)
        .text_size(dp(11.5))
        .child(div().flex_none().size(dp(6.)).rounded_full().bg(color))
        .child(div().whitespace_nowrap().child(value.to_string()))
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
/// Called with (column, typed text) while a foreign-key cell is edited; the
/// host looks up matching rows and puts them in `fk_matches`.
pub type FkSearchFn = Rc<dyn Fn(usize, String, &mut Window, &mut App)>;

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
    /// The month the date editor's calendar shows; `None` follows the value.
    pub cal_month: Option<chrono::NaiveDate>,
    pub on_fk_search: Option<FkSearchFn>,
    /// Rows of the referenced table matching the foreign-key editor's text:
    /// (key, label), and which one ↑ / ↓ highlight. `None` while looking.
    pub fk_matches: Option<Vec<(String, String)>>,
    pub fk_highlight: Option<usize>,
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
        // Booleans edit with a true / false (/ NULL) dropdown, in the
        // spelling the column already uses.
        for c in 0..self.columns.len() {
            if kind_of_type(&self.types[c]) != Some(Kind::Bool)
                || self.choices.get(c).is_some_and(|o| !o.is_empty())
            {
                continue;
            }
            let numeric = self
                .rows
                .iter()
                .take(200)
                .filter_map(|r| r[c].as_deref())
                .any(|v| v == "1" || v == "0")
                || self.types[c].to_ascii_lowercase().starts_with("tinyint")
                || self.types[c].to_ascii_lowercase().starts_with("bit");
            let (yes, no) = if numeric {
                ("1", "0")
            } else {
                ("true", "false")
            };
            let nullable = table
                .iter()
                .find(|t| t.name == self.columns[c])
                .is_none_or(|t| t.nullable);
            if let Some(slot) = self.choices.get_mut(c) {
                *slot = vec![Some(yes.into()), Some(no.into())];
                if nullable {
                    slot.push(None);
                }
            }
        }
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
                let typed = self.types.get(c).and_then(|t| kind_of_type(t));
                if typed == Some(Kind::Bool) {
                    return Kind::Bool;
                }
                if self.choices.get(c).is_some_and(|ch| !ch.is_empty()) {
                    return Kind::Enum;
                }
                if let Some(k) = typed {
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
        // Pills need room for their dot and padding.
        for c in 0..self.columns.len() {
            if self.kinds[c] != Kind::Enum {
                continue;
            }
            let longest = self
                .rows
                .iter()
                .take(100)
                .filter_map(|r| r[c].as_deref())
                .chain(self.choices[c].iter().flatten().map(String::as_str))
                .map(|v| v.chars().take(40).count())
                .max()
                .unwrap_or(0);
            if let Some(w) = self.widths.get_mut(c) {
                *w = w.max((longest as f32 * 7.4 + 56.).min(380.));
            }
        }
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

impl Grid {
    /// The cell being edited: a 2px accent border (red while the value is
    /// invalid), the editor, and under the cell what's wrong or a hint
    /// (design.md, Cell editors #1–4, #10).
    fn render_editor(
        &self,
        col_ix: usize,
        editor: &CellEditor,
        cx: &mut Context<TableState<Self>>,
    ) -> AnyElement {
        let t = tokens(cx);
        let cell = h_flex()
            .size_full()
            .relative()
            .key_context("CellEditor")
            .bg(t.canvas)
            .border_2();
        let input = match editor {
            CellEditor::Choice(choices) => {
                return cell
                    .border_color(t.accent)
                    .child(choices.clone())
                    .into_any_element();
            }
            CellEditor::Text(input) => input.clone(),
        };
        let ty = self.types.get(col_ix).cloned().unwrap_or_default();
        let checked = check(&ty, &input.read(cx).value());
        let uuid = ty.eq_ignore_ascii_case("uuid");
        let note = checked
            .error
            .clone()
            .map(|e| (e, true))
            .or(checked.hint.clone().map(|h| (h, false)));
        let stepper = input.clone();
        let fk = self.links.get(col_ix).copied().unwrap_or(false) && self.on_fk_search.is_some();
        let fk_list = fk.then(|| self.render_fk_matches(col_ix, &input, cx));
        let calendar = temporal(&ty)
            .filter(|k| *k != Temporal::Time)
            .map(|k| self.render_calendar(k, &input, &checked, cx));
        cell.border_color(if checked.error.is_some() {
            t.danger
        } else {
            t.accent
        })
        .font_family(cx.theme().mono_font_family.clone())
        .when_some(fk_list, |d, list| {
            d.child(
                div().absolute().left(px(-2.)).top_full().child(
                    gpui::deferred(
                        gpui::anchored()
                            .snap_to_window_with_margin(px(8.))
                            .child(list),
                    )
                    .with_priority(1),
                ),
            )
        })
        // ↑ / ↓: through foreign-key matches, or step a number (⇧ by 10).
        .on_action(cx.listener({
            let input = stepper.clone();
            move |table, _: &CellUp, window, cx| {
                table.delegate_mut().arrow(-1, false, &input, window, cx)
            }
        }))
        .on_action(cx.listener({
            let input = stepper.clone();
            move |table, _: &CellDown, window, cx| {
                table.delegate_mut().arrow(1, false, &input, window, cx)
            }
        }))
        .on_action(cx.listener({
            let input = stepper.clone();
            move |table, _: &CellUpMore, window, cx| {
                table.delegate_mut().arrow(-1, true, &input, window, cx)
            }
        }))
        .on_action(cx.listener({
            let input = stepper.clone();
            move |table, _: &CellDownMore, window, cx| {
                table.delegate_mut().arrow(1, true, &input, window, cx)
            }
        }))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .h_full()
                .child(Input::new(&input).appearance(false).h_full()),
        )
        .when(uuid, |d| {
            let target = input.clone();
            d.child(
                div()
                    .id("uuid-generate")
                    .debug_selector(|| "uuid-generate".into())
                    .flex_none()
                    .mr(dp(4.))
                    .px(dp(6.))
                    .h(dp(20.))
                    .flex()
                    .items_center()
                    .gap(dp(4.))
                    .rounded(dp(5.))
                    .bg(t.surface)
                    .border_1()
                    .border_color(t.border)
                    .cursor_pointer()
                    .font_family(cx.theme().font_family.clone())
                    .text_size(dp(11.))
                    .text_color(t.text_secondary)
                    .hover(|d| d.text_color(t.text))
                    .child(Icon::new(IconName::Sparkles).size(dp(11.)))
                    .child("Generate")
                    // Mouse-down, so the editor keeps focus (and isn't saved first).
                    .on_mouse_down(gpui::MouseButton::Left, move |_, window, cx| {
                        cx.stop_propagation();
                        window.prevent_default();
                        let v = uuid::Uuid::new_v4().to_string();
                        target.update(cx, |i, cx| i.set_value(v, window, cx));
                    }),
            )
        })
        .when_some(calendar, |d, cal| {
            d.child(
                div().absolute().left(px(-2.)).top_full().child(
                    gpui::deferred(
                        gpui::anchored()
                            .snap_to_window_with_margin(px(8.))
                            .child(cal),
                    )
                    .with_priority(1),
                ),
            )
        })
        .when(temporal(&ty) == Some(Temporal::Time), |d| {
            let target = input.clone();
            d.child(
                div()
                    .id("time-now")
                    .debug_selector(|| "time-now".into())
                    .flex_none()
                    .mr(dp(4.))
                    .px(dp(6.))
                    .h(dp(20.))
                    .flex()
                    .items_center()
                    .rounded(dp(5.))
                    .bg(t.surface)
                    .border_1()
                    .border_color(t.border)
                    .cursor_pointer()
                    .font_family(cx.theme().font_family.clone())
                    .text_size(dp(11.))
                    .text_color(t.text_secondary)
                    .child("Now")
                    .on_mouse_down(gpui::MouseButton::Left, move |_, window, cx| {
                        cx.stop_propagation();
                        window.prevent_default();
                        let v = now_for(Temporal::Time);
                        target.update(cx, |i, cx| i.set_value(v, window, cx));
                    }),
            )
        })
        .when_some(
            note.filter(|_| !fk && temporal(&ty).is_none_or(|k| k == Temporal::Time)),
            |d, (text, error)| {
                d.child(
                    div().absolute().left(px(-2.)).top_full().child(
                        gpui::deferred(
                            gpui::anchored().snap_to_window_with_margin(px(8.)).child(
                                h_flex()
                                    .mt(dp(6.))
                                    .max_w(dp(360.))
                                    .px(dp(8.))
                                    .py(dp(5.))
                                    .gap(dp(6.))
                                    .rounded(dp(6.))
                                    .bg(if error {
                                        t.danger.opacity(0.14).blend(t.raised)
                                    } else {
                                        t.raised
                                    })
                                    .border_1()
                                    .border_color(if error { t.danger } else { t.border })
                                    .shadow_md()
                                    .font_family(cx.theme().font_family.clone())
                                    .text_size(dp(11.5))
                                    .text_color(if error { t.danger } else { t.text_secondary })
                                    .when(error, |d| {
                                        d.child(Icon::new(IconName::CircleAlert).size(dp(12.)))
                                    })
                                    .child(text),
                            ),
                        )
                        .with_priority(1),
                    ),
                )
            },
        )
        .into_any_element()
    }
}

impl Grid {
    /// ↑ (`dir` -1) / ↓ (+1) in the text editor: move the foreign-key match
    /// highlight when there are matches, else step a number column's value
    /// (by 10 with ⇧). Anything else is left to the text field.
    fn arrow(
        &mut self,
        dir: i32,
        more: bool,
        input: &Entity<gpui_component::input::InputState>,
        window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) {
        let Some(c) = self.editing.as_ref().map(|(_, c, _)| *c) else {
            return;
        };
        let n = self.fk_matches.as_ref().map_or(0, Vec::len);
        if self.links.get(c).copied().unwrap_or(false) && n > 0 && !more {
            self.fk_highlight = match (dir, self.fk_highlight) {
                (1, None) => Some(0),
                (1, Some(i)) => Some((i + 1).min(n - 1)),
                (_, Some(0)) | (_, None) => None,
                (_, Some(i)) => Some(i - 1),
            };
            cx.notify();
            return;
        }
        if self.kind(c) == Kind::Number {
            let now = input.read(cx).value().trim().to_string();
            if let Ok(v) = now.parse::<f64>() {
                let decimals = now.split_once('.').map_or(0, |(_, f)| f.len());
                // ↑ adds, ↓ subtracts.
                let step = if more { 10. } else { 1. } * -(dir as f64);
                let next = format!("{:.*}", decimals, v + step);
                input.update(cx, |i, cx| i.set_value(next, window, cx));
                return;
            }
        }
        cx.propagate();
    }

    /// Matching rows of the referenced table under a foreign-key cell: key
    /// and label; a click (or ↑ / ↓ and ↵) picks one. Says so when the typed
    /// key matches no row (it can still be saved).
    fn render_fk_matches(
        &self,
        col_ix: usize,
        input: &Entity<gpui_component::input::InputState>,
        cx: &mut Context<TableState<Self>>,
    ) -> gpui::AnyElement {
        let t = tokens(cx);
        let mono = cx.theme().mono_font_family.clone();
        let text = input.read(cx).value().trim().to_string();
        let target = self.refs.get(col_ix).cloned().flatten().unwrap_or_default();
        let checked = check(&self.types.get(col_ix).cloned().unwrap_or_default(), &text);
        let body = match &self.fk_matches {
            None => vec![
                div()
                    .px(dp(8.))
                    .py(dp(6.))
                    .text_color(t.text_muted)
                    .child("Looking…")
                    .into_any_element(),
            ],
            Some(m) if m.is_empty() => vec![
                div()
                    .px(dp(8.))
                    .py(dp(6.))
                    .text_color(t.text_muted)
                    .child("No matching rows")
                    .into_any_element(),
            ],
            Some(m) => m
                .iter()
                .enumerate()
                .map(|(i, (key, label))| {
                    let (input, key2) = (input.clone(), key.clone());
                    h_flex()
                        .id(("fk-match", i))
                        .debug_selector(move || format!("fk-match-{i}"))
                        .h(dp(30.))
                        .px(dp(8.))
                        .gap(dp(10.))
                        .rounded(dp(6.))
                        .cursor_pointer()
                        .when(self.fk_highlight == Some(i), |d| {
                            d.bg(t.accent.opacity(0.16))
                        })
                        .hover(|d| d.bg(t.hover))
                        .child(
                            div()
                                .flex_none()
                                .font_family(mono.clone())
                                .text_color(t.info)
                                .child(key.clone()),
                        )
                        .child(
                            div()
                                .min_w_0()
                                .truncate()
                                .text_color(t.text_secondary)
                                .child(label.clone()),
                        )
                        .on_mouse_down(gpui::MouseButton::Left, move |_, window, cx| {
                            cx.stop_propagation();
                            window.prevent_default();
                            input.update(cx, |i, cx| i.set_value(key2.clone(), window, cx));
                        })
                        .into_any_element()
                })
                .collect(),
        };
        let unknown = !text.is_empty()
            && checked.error.is_none()
            && self
                .fk_matches
                .as_ref()
                .is_some_and(|m| !m.iter().any(|(k, _)| *k == text));
        v_flex()
            .occlude()
            .mt(dp(8.))
            .p(dp(5.))
            .w(dp(340.))
            .rounded(dp(10.))
            .bg(t.raised)
            .border_1()
            .border_color(t.border)
            .shadow_lg()
            .font_family(cx.theme().font_family.clone())
            .text_size(dp(12.))
            .child(
                h_flex()
                    .px(dp(8.))
                    .pb(dp(4.))
                    .gap(dp(6.))
                    .text_size(dp(11.))
                    .text_color(t.text_muted)
                    .child(div().text_color(t.info).child(format!("→ {target}")))
                    .child("matching key or name"),
            )
            .when_some(checked.error.clone(), |d, e| {
                d.child(
                    h_flex()
                        .px(dp(8.))
                        .py(dp(4.))
                        .gap(dp(6.))
                        .text_color(t.danger)
                        .child(Icon::new(IconName::CircleAlert).size(dp(12.)))
                        .child(e),
                )
            })
            .children(body)
            .child(
                div()
                    .mt(dp(4.))
                    .pt(dp(6.))
                    .px(dp(8.))
                    .border_t_1()
                    .border_color(t.border)
                    .text_size(dp(11.))
                    .text_color(if unknown { t.warning } else { t.text_faint })
                    .child(if unknown {
                        format!("No {target} row with this key; it can still be saved")
                    } else {
                        "↑↓ choose · ↵ saves · click picks".to_string()
                    }),
            )
            .into_any_element()
    }

    /// The date editor's month: pick a day (the time of day is kept), page
    /// months, Today / Now. Any error shows on top. The text stays editable.
    fn render_calendar(
        &self,
        kind: Temporal,
        input: &Entity<gpui_component::input::InputState>,
        checked: &crate::ui::cell_value::Check,
        cx: &mut Context<TableState<Self>>,
    ) -> gpui::AnyElement {
        use chrono::Datelike as _;
        let t = tokens(cx);
        let text = input.read(cx).value().to_string();
        let today = chrono::Local::now().date_naive();
        let picked = date_part(&text);
        let month = self
            .cal_month
            .or(picked)
            .unwrap_or(today)
            .with_day(1)
            .unwrap_or(today);
        let shift = |m: chrono::NaiveDate, by: i32| {
            let n = m.year() * 12 + m.month0() as i32 + by;
            chrono::NaiveDate::from_ymd_opt(n.div_euclid(12), n.rem_euclid(12) as u32 + 1, 1)
                .unwrap_or(m)
        };
        // Mondays first; blanks before the 1st.
        let lead = month.weekday().num_days_from_monday() as usize;
        let days = (shift(month, 1) - month).num_days() as usize;
        let nav =
            |id: &'static str, icon: IconName, by: i32, cx: &mut Context<TableState<Self>>| {
                div()
                    .id(id)
                    .size(dp(22.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(dp(5.))
                    .cursor_pointer()
                    .hover(|d| d.bg(t.hover))
                    .child(Icon::new(icon).size(dp(13.)))
                    .on_mouse_down(
                        gpui::MouseButton::Left,
                        cx.listener(move |table, _, window, cx| {
                            cx.stop_propagation();
                            window.prevent_default();
                            table.delegate_mut().cal_month = Some(shift(month, by));
                            cx.notify();
                        }),
                    )
            };
        let set = |v: String, input: Entity<gpui_component::input::InputState>| {
            move |_: &gpui::MouseDownEvent, window: &mut Window, cx: &mut App| {
                cx.stop_propagation();
                window.prevent_default();
                input.update(cx, |i, cx| i.set_value(v.clone(), window, cx));
            }
        };
        let mut grid = vec![];
        for _ in 0..lead {
            grid.push(div().size(dp(28.)).into_any_element());
        }
        for day in 1..=days {
            let date = month.with_day(day as u32).unwrap_or(month);
            let selected = picked == Some(date);
            grid.push(
                div()
                    .id(("cal-day", day))
                    .debug_selector(move || format!("cal-day-{day}"))
                    .size(dp(28.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(dp(6.))
                    .cursor_pointer()
                    .map(|d| {
                        if selected {
                            d.bg(t.accent).text_color(t.canvas)
                        } else if date == today {
                            d.border_1().border_color(t.accent).text_color(t.text)
                        } else {
                            d.text_color(t.text).hover(|d| d.bg(t.hover))
                        }
                    })
                    .child(day.to_string())
                    .on_mouse_down(
                        gpui::MouseButton::Left,
                        set(with_date(&text, date, kind), input.clone()),
                    )
                    .into_any_element(),
            );
        }
        let button = |id: &'static str, label: &'static str| {
            div()
                .id(id)
                .debug_selector(move || id.into())
                .h(dp(24.))
                .px(dp(8.))
                .flex()
                .items_center()
                .rounded(dp(6.))
                .bg(t.surface)
                .border_1()
                .border_color(t.border)
                .cursor_pointer()
                .text_color(t.text_secondary)
                .hover(|d| d.text_color(t.text))
                .child(label)
        };
        v_flex()
            .occlude()
            .mt(dp(8.))
            .p(dp(10.))
            .gap(dp(8.))
            .w(dp(240.))
            .rounded(dp(10.))
            .bg(t.raised)
            .border_1()
            .border_color(t.border)
            .shadow_lg()
            .font_family(cx.theme().font_family.clone())
            .text_size(dp(12.))
            .when_some(checked.error.clone(), |d, e| {
                d.child(
                    h_flex()
                        .gap(dp(6.))
                        .text_color(t.danger)
                        .child(Icon::new(IconName::CircleAlert).size(dp(12.)))
                        .child(e),
                )
            })
            .child(
                h_flex()
                    .justify_between()
                    .child(nav("cal-prev", IconName::ChevronLeft, -1, cx))
                    .child(
                        div()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(month.format("%B %Y").to_string()),
                    )
                    .child(nav("cal-next", IconName::ChevronRight, 1, cx)),
            )
            .child(
                h_flex().children(["Mo", "Tu", "We", "Th", "Fr", "Sa", "Su"].map(|d| {
                    div()
                        .w(dp(28.))
                        .flex()
                        .justify_center()
                        .text_size(dp(10.5))
                        .text_color(t.text_muted)
                        .child(d)
                })),
            )
            .child(h_flex().flex_wrap().w(dp(196.)).children(grid))
            .child(
                h_flex()
                    .gap(dp(6.))
                    .pt(dp(8.))
                    .border_t_1()
                    .border_color(t.border)
                    .child(button("cal-today", "Today").on_mouse_down(
                        gpui::MouseButton::Left,
                        set(with_date(&text, today, kind), input.clone()),
                    ))
                    .when(kind == Temporal::DateTime, |d| {
                        d.child(button("cal-now", "Now").on_mouse_down(
                            gpui::MouseButton::Left,
                            set(now_for(kind), input.clone()),
                        ))
                    })
                    .child(div().flex_1())
                    .child(
                        div()
                            .text_size(dp(11.))
                            .text_color(t.text_faint)
                            .child("↵ saves"),
                    ),
            )
            .into_any_element()
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
        } else if row_ix >= self.base_rows && row_ix < self.rows.len() {
            // (The table also asks for its empty filler rows past the end.)
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
        if let Some((r, c, editor)) = &self.editing
            && *r == row_ix
            && *c == col_ix
        {
            return self.render_editor(col_ix, editor, cx);
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
                Kind::Array => {
                    let items = array_items(v);
                    let more = items.len().saturating_sub(3);
                    h_flex()
                        .gap(dp(4.))
                        .min_w_0()
                        .children(items.into_iter().take(3).map(|item| {
                            div()
                                .flex_none()
                                .px(dp(5.))
                                .h(dp(18.))
                                .flex()
                                .items_center()
                                .rounded(dp(4.))
                                .bg(t.hover)
                                .text_size(dp(11.))
                                .text_color(t.text_secondary)
                                .child(item)
                        }))
                        .when(more > 0, |d| {
                            d.child(div().text_color(t.text_faint).child(format!("+{more}")))
                        })
                        .into_any_element()
                }
                Kind::Binary => {
                    let ty = self.types.get(col_ix).cloned().unwrap_or_default();
                    let size = hex_len(v).unwrap_or(v.len());
                    h_flex()
                        .gap(dp(6.))
                        .text_color(t.text_muted)
                        .child(Icon::new(IconName::File).size(dp(12.)))
                        .child(format!("{} · {}", ty.to_lowercase(), human_size(size)))
                        .into_any_element()
                }
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
    fn arrays_and_binary() {
        assert_eq!(array_items(r#"{a,"b c",NULL}"#), ["a", "b c", "NULL"]);
        assert_eq!(array_items(r#"{"x\"y"}"#), [r#"x"y"#]);
        assert_eq!(array_items("{}"), Vec::<String>::new());
        assert_eq!(array_items("red,blue"), ["red", "blue"]);
        assert_eq!(kind_of_type("text[]"), Some(Kind::Array));
        assert_eq!(kind_of_type("set('a','b')"), Some(Kind::Array));
        assert_eq!(kind_of_type("bytea"), Some(Kind::Binary));
        assert_eq!(hex_len("\\x0a1b"), Some(2));
        assert_eq!(hex_len("0x0a1b2c"), Some(3));
        assert_eq!(hex_len("hello"), None);
        assert_eq!(human_size(24_100), "23.5 KB");
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
