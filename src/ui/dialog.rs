//! The shared dialog look (design.md, Components → Dialog): `bg/sidebar`,
//! radius 14, 24px inset, a 17/600 title, and a footer under a 1px line with
//! the actions on the right. Plus the controls dialogs share: section
//! labels, segmented controls and selectable tiles.

use std::rc::Rc;

use gpui::{
    AnyElement, App, Div, ElementId, FontWeight, IntoElement, ParentElement as _, Pixels,
    SharedString, Stateful, Styled as _, Window, div, prelude::*, px,
};
use gpui_component::dialog::Dialog;
use gpui_component::{Icon, Sizable as _, h_flex, v_flex};
use gpui_kit_assets::IconName;

use super::theme::{caps, dp, tokens};

/// A dialog `px` design pixels wide (at the default 13px interface size).
pub fn width(px_: f32, cx: &App) -> Pixels {
    px(px_ * crate::settings::get(cx).ui_font_size / 13.)
}

/// Gives a dialog the shared surface. The content brings its own title
/// (`title`) and footer (`footer`).
pub fn shell(dialog: Dialog, w: Pixels, cx: &App) -> Dialog {
    let t = tokens(cx);
    dialog
        .w(w)
        .close_button(false)
        // Enter belongs to the fields (a URL field applies on Enter); a
        // stray Enter must never close a form and lose what was typed.
        .on_ok(|_, _, _| false)
        .bg(t.sidebar)
        .border_color(t.border)
        .rounded(dp(14.))
        .pt(dp(22.))
        .pb(dp(18.))
        .px(dp(24.))
}

/// "Export" with an optional muted line under it.
pub fn title(text: impl Into<SharedString>, sub: Option<SharedString>, cx: &App) -> Div {
    let t = tokens(cx);
    v_flex()
        .gap(dp(4.))
        .child(
            div()
                .text_size(dp(17.))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(t.text)
                .child(text.into()),
        )
        .when_some(sub, |d, sub| {
            d.child(
                div()
                    .text_size(dp(12.5))
                    .text_color(t.text_muted)
                    .child(sub),
            )
        })
}

/// The footer row: a faint hint on the left, actions on the right.
pub fn footer(hint: impl Into<SharedString>, actions: Vec<AnyElement>, cx: &App) -> Div {
    let t = tokens(cx);
    h_flex()
        .pt(dp(14.))
        .gap(dp(8.))
        .border_t_1()
        .border_color(t.border)
        .child(
            div()
                .text_size(dp(11.5))
                .text_color(t.text_faint)
                .child(hint.into()),
        )
        .child(div().flex_1())
        .children(actions)
}

/// A labelled block: caps label, then the content.
pub fn section(label: &str, content: impl IntoElement, cx: &App) -> Div {
    v_flex().gap(dp(8.)).child(caps(label, cx)).child(content)
}

pub type OnSelect = Rc<dyn Fn(usize, &mut Window, &mut App)>;

/// A segmented control (design.md, Components): `bg/surface`, 1px border,
/// 3px inset; the active segment is `bg/hover` with primary text.
pub fn segmented(
    id: &'static str,
    labels: &[&str],
    selected: usize,
    on_select: OnSelect,
    cx: &App,
) -> Stateful<Div> {
    let t = tokens(cx);
    h_flex()
        .id(id)
        .p(dp(3.))
        .gap(dp(2.))
        .rounded(dp(7.))
        .bg(t.surface)
        .border_1()
        .border_color(t.border)
        .children(labels.iter().enumerate().map(|(i, label)| {
            let on_select = on_select.clone();
            let active = i == selected;
            div()
                .id(ElementId::NamedInteger(id.into(), i as u64))
                .debug_selector(move || format!("{id}-{i}"))
                .flex_1()
                .flex()
                .justify_center()
                .items_center()
                .h(dp(24.))
                .px(dp(10.))
                .rounded(dp(5.))
                .cursor_pointer()
                .text_size(dp(12.))
                .when(active, |d| {
                    d.bg(t.hover)
                        .text_color(t.text)
                        .font_weight(FontWeight::MEDIUM)
                })
                .when(!active, |d| {
                    d.text_color(t.text_secondary)
                        .hover(|s| s.text_color(t.text))
                })
                .on_click(move |_, window, cx| on_select(i, window, cx))
                .child(label.to_string())
        }))
}

/// A selectable tile: icon, label and a muted line (format and engine
/// tiles). Selected = accent border and a faint accent fill.
pub fn tile(
    id: impl Into<SharedString>,
    icon: Option<IconName>,
    label: impl Into<SharedString>,
    sub: impl Into<SharedString>,
    selected: bool,
    cx: &App,
) -> Stateful<Div> {
    let t = tokens(cx);
    let id: SharedString = id.into();
    v_flex()
        .id(ElementId::Name(id.clone()))
        .debug_selector(move || id.to_string())
        .gap(dp(4.))
        .px(dp(12.))
        .py(dp(10.))
        .rounded(dp(9.))
        .border_1()
        .cursor_pointer()
        .map(|d| {
            if selected {
                d.border_color(t.accent).bg(t.accent.opacity(0.10))
            } else {
                d.border_color(t.border)
                    .bg(t.surface)
                    .hover(|s| s.bg(t.hover))
            }
        })
        .child(
            h_flex()
                .gap(dp(8.))
                .text_size(dp(12.5))
                .font_weight(FontWeight::MEDIUM)
                .text_color(if selected { t.text } else { t.text_secondary })
                .when_some(icon, |d, icon| {
                    d.child(Icon::new(icon).small().text_color(if selected {
                        t.accent
                    } else {
                        t.text_muted
                    }))
                })
                .child(label.into()),
        )
        .child(
            div()
                .text_size(dp(11.))
                .text_color(t.text_muted)
                .child(sub.into()),
        )
}

/// A radio-style row: a ring that fills when selected, a label and a muted
/// count on the right.
pub fn choice(
    id: impl Into<SharedString>,
    label: impl Into<SharedString>,
    detail: Option<SharedString>,
    selected: bool,
    cx: &App,
) -> Stateful<Div> {
    let t = tokens(cx);
    let id: SharedString = id.into();
    h_flex()
        .id(ElementId::Name(id.clone()))
        .debug_selector(move || id.to_string())
        .gap(dp(10.))
        .h(dp(32.))
        .px(dp(10.))
        .rounded(dp(7.))
        .cursor_pointer()
        .when(selected, |d| d.bg(t.hover))
        .when(!selected, |d| d.hover(|s| s.bg(t.hover.opacity(0.6))))
        .child(
            div()
                .flex()
                .items_center()
                .justify_center()
                .size(dp(14.))
                .rounded_full()
                .border_1()
                .border_color(if selected { t.accent } else { t.border })
                .when(selected, |d| {
                    d.child(div().size(dp(6.)).rounded_full().bg(t.accent))
                }),
        )
        .child(
            div()
                .text_size(dp(12.5))
                .text_color(if selected { t.text } else { t.text_secondary })
                .child(label.into()),
        )
        .child(div().flex_1())
        .when_some(detail, |d, detail| {
            d.child(
                div()
                    .text_size(dp(11.5))
                    .text_color(t.text_muted)
                    .child(detail),
            )
        })
}
