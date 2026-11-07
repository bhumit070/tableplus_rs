//! Row detail side panel (⌘⇧I) with three tabs (design.md screen 01):
//! Fields — every field of the selected row, one per line, JSON values
//! pretty-printed, a pencil to edit the cell and a copy button; JSON — the
//! row as one object; References — the row's foreign keys, each opening
//! the referenced row.

use gpui::{
    AnyElement, App, ClipboardItem, Context, Global, IntoElement, ParentElement, Styled, div,
    prelude::*,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::{ActiveTheme as _, Icon, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit_assets::IconName;

use super::cell_edit::{self, CellEditHost, EditStart};
use gpui::FontWeight;
use serde_json::{Map, Value, from_str, to_string_pretty};

use super::theme::{dp, tokens};

/// The panel's tab, shared by every grid so it stays put between rows.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub enum PanelTab {
    #[default]
    Fields,
    Json,
    References,
}

impl Global for PanelTab {}

/// `v` re-indented, when it's a JSON object or array.
pub fn pretty_json(v: &str) -> Option<String> {
    let t = v.trim_start();
    if !(t.starts_with('{') || t.starts_with('[')) {
        return None;
    }
    let parsed: Value = from_str(v).ok()?;
    to_string_pretty(&parsed).ok()
}

/// The row shown: the selected row, or the selected cell's row.
pub fn current_row<H: CellEditHost>(host: &H, cx: &App) -> Option<usize> {
    let t = host.grid().read(cx);
    t.selected_row().or(t.selected_cell().map(|(r, _)| r))
}

/// `types[i]` is column i's type, when known.
pub fn render<H: CellEditHost>(
    host: &H,
    types: &[Option<String>],
    cx: &mut Context<H>,
) -> AnyElement {
    let theme = cx.theme().clone();
    let t = tokens(cx);
    let tab = cx.try_global::<PanelTab>().copied().unwrap_or_default();
    let row = current_row(host, cx);
    let g = host.grid().read(cx).delegate();
    let shown_row = row.filter(|r| *r < g.rows.len());
    let body = match shown_row {
        Some(r) if tab == PanelTab::Json => {
            // The row as one JSON object (as "Copy as JSON" makes it).
            let object: Map<String, Value> = g
                .columns
                .iter()
                .enumerate()
                .map(|(c, name)| {
                    // JSON columns nest as JSON, not as an escaped string.
                    let value = match g.cell_string(r, c) {
                        None => Value::Null,
                        Some(text) => pretty_json(&text)
                            .and_then(|_| from_str(&text).ok())
                            .unwrap_or(Value::String(text)),
                    };
                    (name.clone(), value)
                })
                .collect();
            let text = to_string_pretty(&Value::Object(object)).unwrap_or_default();
            let copy = text.clone();
            v_flex()
                .p(dp(10.))
                .gap(dp(8.))
                .child(
                    h_flex().child(div().flex_1()).child(
                        Button::new("copy-row-json")
                            .cursor_pointer()
                            .icon(IconName::Copy)
                            .label("Copy")
                            .ghost()
                            .xsmall()
                            .on_click(move |_, window, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(copy.clone()));
                                window.push_notification("Row copied as JSON", cx);
                            }),
                    ),
                )
                .child(
                    div()
                        .font_family(theme.mono_font_family.clone())
                        .text_size(dp(11.5))
                        .text_color(t.text_secondary)
                        .children(
                            text.lines()
                                .map(|line| div().child(line.to_string()))
                                .collect::<Vec<_>>(),
                        ),
                )
                .into_any_element()
        }
        Some(r) if tab == PanelTab::References => {
            let links: Vec<(usize, String, String)> = g
                .refs
                .iter()
                .enumerate()
                .filter_map(|(c, target)| {
                    let target = target.clone()?;
                    let value = g.cell_string(r, c)?;
                    Some((c, target, value))
                })
                .collect();
            let follow = g.on_follow.clone();
            if links.is_empty() {
                div()
                    .p(dp(12.))
                    .text_size(dp(12.5))
                    .text_color(t.text_muted)
                    .child("This row has no foreign keys set.")
                    .into_any_element()
            } else {
                v_flex()
                    .p(dp(6.))
                    .gap(dp(2.))
                    .children(links.into_iter().map(|(c, target, value)| {
                        let follow = follow.clone();
                        h_flex()
                            .id(("row-ref", c))
                            .gap(dp(8.))
                            .px(dp(8.))
                            .h(dp(32.))
                            .rounded(dp(6.))
                            .cursor_pointer()
                            .hover(|style| style.bg(t.hover))
                            .text_size(dp(12.5))
                            .child(
                                div()
                                    .text_color(t.text_secondary)
                                    .child(g.columns[c].clone()),
                            )
                            .child(div().flex_1())
                            .child(
                                div()
                                    .font_family(theme.mono_font_family.clone())
                                    .text_color(t.info)
                                    .child(format!("{target} #{value}")),
                            )
                            .child(Icon::new(IconName::ArrowRight).xsmall().text_color(t.info))
                            .on_click(move |_, window, cx| {
                                if let Some(follow) = &follow {
                                    follow(r, c, window, cx);
                                }
                            })
                    }))
                    .into_any_element()
            }
        }
        _ => match shown_row {
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
                    let shown = value
                        .as_deref()
                        .map(|v| pretty_json(v).unwrap_or_else(|| v.to_string()));
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
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .child(name),
                                )
                                .when_some(ty, |d, t| {
                                    d.child(
                                        div().text_xs().text_color(theme.muted_foreground).child(t),
                                    )
                                })
                                .child(div().flex_1())
                                .when(editable, |d| {
                                    d.child(
                                        Button::new(("detail-edit", c))
                                            .cursor_pointer()
                                            .debug_selector(move || format!("detail-edit-{c}"))
                                            .icon(IconName::Replace)
                                            .ghost()
                                            .xsmall()
                                            .tooltip("Edit")
                                            .on_click(cx.listener(
                                                move |h: &mut H, _, window, cx| {
                                                    h.grid().update(cx, |t, cx| {
                                                        t.set_selected_cell(r, c, cx)
                                                    });
                                                    cell_edit::start_edit(
                                                        h,
                                                        r,
                                                        c,
                                                        EditStart::Keep,
                                                        window,
                                                        cx,
                                                    );
                                                },
                                            )),
                                    )
                                })
                                .child(
                                    Button::new(("detail-copy", c))
                                        .cursor_pointer()
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
        },
    };
    let tab_button = |label: &'static str, which: PanelTab| {
        let active = tab == which;
        div()
            .id(label)
            .debug_selector(move || format!("panel-tab-{label}"))
            .px(dp(8.))
            .h(dp(24.))
            .flex()
            .items_center()
            .rounded(dp(5.))
            .cursor_pointer()
            .text_size(dp(12.))
            .when(active, |d| d.bg(t.hover).text_color(t.text))
            .when(!active, |d| {
                d.text_color(t.text_muted).hover(|s| s.text_color(t.text))
            })
            .child(label)
            .on_click(move |_, _, cx| cx.set_global(which))
    };
    v_flex()
        .w(dp(340.))
        .h_full()
        .flex_none()
        .border_l_1()
        .border_color(theme.border)
        .child(
            h_flex()
                .px(dp(8.))
                .h(dp(36.))
                .gap(dp(2.))
                .border_b_1()
                .border_color(theme.border)
                .child(tab_button("Fields", PanelTab::Fields))
                .child(tab_button("JSON", PanelTab::Json))
                .child(tab_button("References", PanelTab::References))
                .child(div().flex_1())
                .child(
                    div()
                        .text_size(dp(11.))
                        .text_color(t.text_faint)
                        .child(match row {
                            Some(r) => format!("row {}", r + 1),
                            None => String::new(),
                        }),
                ),
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
