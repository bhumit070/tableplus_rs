//! MongoDB's extra data views (design.md screen 20): **Documents**, one
//! card per document (the default), and **JSON**, the page as raw JSON.
//! Both read the grid, so staged edits show in every view and save the
//! same way (`$set` by `_id`). Clicking a card's value edits it in the
//! JSON / long-text dialog.

use gpui::{
    AnyElement, Context, Entity, FontWeight, IntoElement, ParentElement, Styled, Window, div,
    prelude::*, transparent_black,
};
use gpui_component::ActiveTheme as _;
use gpui_component::input::{Editor, EditorState};
use gpui_component::table::TableState;
use gpui_component::{h_flex, v_flex};
use serde_json::{Map, Value, from_str, to_string_pretty};

use super::cell_edit::{CellEditHost, edit_in_dialog};
use super::grid::Grid;
use super::row_panel::pretty_json;
use super::theme::{dp, tokens};

/// Which view a MongoDB collection's data is in.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DocView {
    #[default]
    Documents,
    Table,
    Json,
}

/// A cell's text as JSON: objects / arrays nest; numbers and booleans
/// (by the field's BSON type) stay numbers and booleans; the rest are
/// strings.
fn json_value(text: Option<String>, bson_type: &str) -> Value {
    let Some(text) = text else {
        return Value::Null;
    };
    let typed = match bson_type {
        "int" | "long" | "double" | "decimal" | "bool" => from_str::<Value>(&text)
            .ok()
            .filter(|value| value.is_number() || value.is_boolean()),
        _ => None,
    };
    typed
        .or_else(|| pretty_json(&text).and_then(|_| from_str(&text).ok()))
        .unwrap_or(Value::String(text))
}

/// The page as a JSON array of documents, staged edits applied and
/// removed documents left out.
pub fn page_json(grid: &Grid) -> String {
    let documents: Vec<Value> = (0..grid.rows.len())
        .filter(|row| !grid.deleted.contains(row))
        .map(|row| {
            let document: Map<String, Value> = grid
                .columns
                .iter()
                .enumerate()
                // A field the document doesn't have is NULL in the table.
                .filter(|(column, _)| {
                    grid.value(row, *column)
                        .is_some_and(|value| value.is_some())
                })
                .map(|(column, name)| {
                    let bson_type = grid
                        .types
                        .get(column)
                        .map(String::as_str)
                        .unwrap_or_default();
                    (
                        name.clone(),
                        json_value(grid.cell_string(row, column), bson_type),
                    )
                })
                .collect();
            Value::Object(document)
        })
        .collect();
    to_string_pretty(&Value::Array(documents)).unwrap_or_default()
}

/// A read-only JSON editor showing `text`.
pub fn json_editor(text: String, window: &mut Window, cx: &mut gpui::App) -> Entity<EditorState> {
    cx.new(|cx| {
        EditorState::new(window, cx)
            .language("json")
            .line_number(true)
            .default_value(text)
    })
}

pub fn render_json(editor: &Entity<EditorState>) -> AnyElement {
    div()
        .size_full()
        .p(dp(10.))
        .child(Editor::new(editor).h_full().readonly(true))
        .into_any_element()
}

/// One card per document on the page.
pub fn render_documents<H: CellEditHost + 'static>(
    table: &Entity<TableState<Grid>>,
    cx: &mut Context<H>,
) -> AnyElement {
    let t = tokens(cx);
    let mono = cx.theme().mono_font_family.clone();
    let grid = table.read(cx).delegate();
    let id_column = grid.columns.iter().position(|column| column == "_id");
    let cards: Vec<AnyElement> = (0..grid.rows.len())
        .map(|row| {
            let removed = grid.deleted.contains(&row);
            let new = row >= grid.base_rows;
            let tint = if removed {
                Some(t.danger)
            } else if new {
                Some(t.success)
            } else {
                None
            };
            let id = id_column
                .and_then(|column| grid.cell_string(row, column))
                .unwrap_or_else(|| "new document".into());
            let fields: Vec<AnyElement> = grid
                .columns
                .iter()
                .enumerate()
                .filter(|(column, _)| Some(*column) != id_column)
                .filter(|(column, _)| {
                    grid.value(row, *column)
                        .is_some_and(|value| value.is_some())
                })
                .map(|(column, name)| {
                    let text = grid.cell_string(row, column).unwrap_or_default();
                    let shown = pretty_json(&text).unwrap_or(text);
                    let edited = grid.edits.contains_key(&(row, column));
                    let lines: Vec<String> = shown.lines().take(12).map(String::from).collect();
                    let more = shown.lines().count() > lines.len();
                    h_flex()
                        .id(("doc-field", row * 1000 + column))
                        .items_start()
                        .gap(dp(12.))
                        .px(dp(8.))
                        .py(dp(3.))
                        .rounded(dp(4.))
                        .cursor_pointer()
                        .when(edited, |line| line.bg(t.warning.opacity(0.12)))
                        .hover(|line| line.bg(t.hover))
                        .child(
                            div()
                                .w(dp(150.))
                                .flex_none()
                                .truncate()
                                .text_color(t.text_muted)
                                .child(name.clone()),
                        )
                        .child(
                            v_flex()
                                .flex_1()
                                .min_w_0()
                                .font_family(mono.clone())
                                .text_size(dp(12.))
                                .children(
                                    lines.into_iter().map(|line| div().truncate().child(line)),
                                )
                                .when(more, |value| {
                                    value.child(div().text_color(t.text_muted).child("…"))
                                }),
                        )
                        .on_click(cx.listener(move |host: &mut H, _, window, cx| {
                            edit_in_dialog(host, row, column, window, cx)
                        }))
                        .into_any_element()
                })
                .collect();
            v_flex()
                .id(("doc-card", row))
                .mx(dp(14.))
                .mb(dp(10.))
                .rounded(dp(8.))
                .border_1()
                .border_color(t.border)
                .border_l_2()
                .border_color(tint.unwrap_or(transparent_black()))
                .bg(t.surface)
                .when(removed, |card| card.opacity(0.5))
                .child(
                    h_flex()
                        .gap(dp(8.))
                        .px(dp(12.))
                        .py(dp(8.))
                        .border_b_1()
                        .border_color(t.border)
                        .text_size(dp(12.))
                        .child(
                            div()
                                .text_color(t.text_muted)
                                .font_weight(FontWeight::SEMIBOLD)
                                .child("_id"),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .font_family(mono.clone())
                                .child(id),
                        )
                        .when(removed, |header| {
                            header.child(div().text_color(t.danger).child("removed"))
                        })
                        .when(new, |header| {
                            header.child(div().text_color(t.success).child("new"))
                        }),
                )
                .child(v_flex().p(dp(4.)).text_size(dp(12.5)).children(fields))
                .into_any_element()
        })
        .collect();
    div()
        .id("documents")
        .size_full()
        .overflow_y_scroll()
        .pt(dp(10.))
        .children(cards)
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn nests_json_fields() {
        assert_eq!(
            json_value(Some("{\"a\":1}".into()), "object"),
            json!({"a": 1})
        );
        assert_eq!(json_value(Some("plain".into()), "string"), json!("plain"));
        assert_eq!(json_value(Some("129".into()), "double"), json!(129));
        assert_eq!(json_value(Some("129".into()), "string"), json!("129"));
        assert_eq!(json_value(Some("true".into()), "bool"), json!(true));
        assert_eq!(json_value(None, "int"), Value::Null);
    }
}
