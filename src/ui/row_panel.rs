//! Row detail side panel (⌘⇧I): every field of the selected row, one per
//! line, with JSON values pretty-printed. A field's pencil edits that cell
//! in the grid; the copy button copies the full value.

use gpui::{
    AnyElement, App, ClipboardItem, Context, IntoElement, ParentElement, Styled, div, prelude::*,
    px,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::{ActiveTheme as _, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit_assets::IconName;

use super::cell_edit::{self, CellEditHost, EditStart};

/// `v` re-indented, when it's a JSON object or array.
pub fn pretty_json(v: &str) -> Option<String> {
    let t = v.trim_start();
    if !(t.starts_with('{') || t.starts_with('[')) {
        return None;
    }
    let parsed: serde_json::Value = serde_json::from_str(v).ok()?;
    serde_json::to_string_pretty(&parsed).ok()
}

/// The row shown: the selected row, or the selected cell's row.
pub fn current_row<H: CellEditHost>(host: &H, cx: &App) -> Option<usize> {
    let t = host.grid().read(cx);
    t.selected_row().or(t.selected_cell().map(|(r, _)| r))
}

/// `types[i]` is column i's type, when known.
pub fn render<H: CellEditHost>(host: &H, types: &[Option<String>], cx: &mut Context<H>) -> AnyElement {
    let theme = cx.theme().clone();
    let row = current_row(host, cx);
    let g = host.grid().read(cx).delegate();
    let body = match row.filter(|r| *r < g.rows.len()) {
        None => div()
            .p_3()
            .text_sm()
            .text_color(theme.muted_foreground)
            .child("Select a row to see its fields.")
            .into_any_element(),
        Some(r) => v_flex()
            .children((0..g.columns.len()).map(|c| {
                let name = g.columns[c].clone();
                let value = g.cell_string(r, c);
                let edited = g.edits.contains_key(&(r, c));
                let editable = g.can_edit(r, c);
                let ty = types.get(c).cloned().flatten();
                let shown = value.as_deref().map(|v| pretty_json(v).unwrap_or_else(|| v.to_string()));
                let copy_value = value.clone().unwrap_or_default();
                v_flex()
                    .px_2()
                    .py_1()
                    .gap_0p5()
                    .border_b_1()
                    .border_color(theme.border)
                    .when(edited, |d| d.bg(theme.warning.opacity(0.18)))
                    .child(
                        h_flex()
                            .gap_1()
                            .child(
                                div()
                                    .text_xs()
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .child(name),
                            )
                            .when_some(ty, |d, t| {
                                d.child(div().text_xs().text_color(theme.muted_foreground).child(t))
                            })
                            .child(div().flex_1())
                            .when(editable, |d| {
                                d.child(
                                    Button::new(("detail-edit", c))
                                        .debug_selector(move || format!("detail-edit-{c}"))
                                        .icon(IconName::Replace)
                                        .ghost()
                                        .xsmall()
                                        .tooltip("Edit")
                                        .on_click(cx.listener(move |h: &mut H, _, window, cx| {
                                            h.grid().update(cx, |t, cx| t.set_selected_cell(r, c, cx));
                                            cell_edit::start_edit(h, r, c, EditStart::Keep, window, cx);
                                        })),
                                )
                            })
                            .child(
                                Button::new(("detail-copy", c))
                                    .icon(IconName::Copy)
                                    .ghost()
                                    .xsmall()
                                    .tooltip("Copy value")
                                    .on_click(move |_, window, cx| {
                                        cx.write_to_clipboard(ClipboardItem::new_string(
                                            copy_value.clone(),
                                        ));
                                        window.push_notification("Copied", cx);
                                    }),
                            ),
                    )
                    .child(match shown {
                        Some(v) => div()
                            .text_sm()
                            .font_family(theme.mono_font_family.clone())
                            .child(v)
                            .into_any_element(),
                        None => div()
                            .text_sm()
                            .italic()
                            .text_color(theme.muted_foreground)
                            .child("NULL")
                            .into_any_element(),
                    })
            }))
            .into_any_element(),
    };
    v_flex()
        .w(px(320.))
        .h_full()
        .flex_none()
        .border_l_1()
        .border_color(theme.border)
        .child(
            div()
                .px_2()
                .py_1()
                .text_xs()
                .text_color(theme.muted_foreground)
                .border_b_1()
                .border_color(theme.border)
                .child(match row {
                    Some(r) => format!("ROW {}", r + 1),
                    None => "ROW".into(),
                }),
        )
        .child(
            div()
                .id("row-detail")
                .debug_selector(|| "row-detail".into())
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .child(body),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_json_documents_are_reformatted() {
        assert_eq!(
            pretty_json(r#"{"a":1,"b":[true,null]}"#).unwrap(),
            "{\n  \"a\": 1,\n  \"b\": [\n    true,\n    null\n  ]\n}"
        );
        assert_eq!(pretty_json("[1,2]").unwrap(), "[\n  1,\n  2\n]");
        assert_eq!(pretty_json("42"), None);
        assert_eq!(pretty_json("{not json"), None);
        assert_eq!(pretty_json("plain text"), None);
    }
}
