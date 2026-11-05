//! Settings dialog: interface and editor font sizes, session restore.

use gpui::{App, Context, IntoElement, ParentElement, Render, Styled, Window, div, prelude::*, px};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::{
    ActiveTheme as _, Disableable as _, Sizable as _, WindowExt as _, h_flex, v_flex,
};
use gpui_kit_assets::IconName;

use crate::settings::{self, EDITOR_RANGE, Settings, UI_RANGE};

pub struct SettingsView;

impl SettingsView {
    pub fn open(window: &mut Window, cx: &mut App) {
        let view = cx.new(|cx| {
            // Re-render when sizes change (here or via ⌘= / ⌘−).
            cx.observe_global::<Settings>(|_, cx| cx.notify()).detach();
            SettingsView
        });
        window.open_dialog(cx, move |dialog, _, _| {
            dialog.title("Settings").w(px(440.)).child(view.clone())
        });
    }
}

/// Label, −, value, + for one size setting.
fn stepper(
    id: &'static str,
    label: &'static str,
    value: f32,
    range: (f32, f32),
    set: fn(&mut Settings, f32),
    cx: &App,
) -> impl IntoElement {
    h_flex()
        .gap_2()
        .child(div().flex_1().text_sm().child(label))
        .child(
            Button::new((id, 0usize))
                .icon(IconName::Minus)
                .small()
                .disabled(value <= range.0)
                .on_click(move |_, _, cx| settings::update(cx, |s| set(s, value - 1.))),
        )
        .child(
            div()
                .w(px(56.))
                .text_center()
                .text_sm()
                .text_color(cx.theme().foreground)
                .child(format!("{value} px")),
        )
        .child(
            Button::new((id, 1usize))
                .icon(IconName::Plus)
                .small()
                .disabled(value >= range.1)
                .on_click(move |_, _, cx| settings::update(cx, |s| set(s, value + 1.))),
        )
}

impl Render for SettingsView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let s = settings::get(cx);
        let theme = cx.theme();
        v_flex()
            .gap_3()
            .child(stepper(
                "ui-size",
                "Interface font size",
                s.ui_font_size,
                UI_RANGE,
                |s, v| s.ui_font_size = v,
                cx,
            ))
            .child(stepper(
                "editor-size",
                "Editor font size",
                s.editor_font_size,
                EDITOR_RANGE,
                |s, v| s.editor_font_size = v,
                cx,
            ))
            .child(
                gpui_component::checkbox::Checkbox::new("restore-sessions")
                    .checked(s.restore_sessions)
                    .label("Reopen last connections and tabs at launch")
                    .on_click(|v: &bool, _, cx| {
                        let v = *v;
                        settings::update(cx, |s| s.restore_sessions = v)
                    }),
            )
            .child(
                div()
                    .p_2()
                    .rounded_md()
                    .bg(theme.muted)
                    .font_family(theme.mono_font_family.clone())
                    .text_size(px(s.editor_font_size))
                    .child("SELECT * FROM users WHERE id = 1;"),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child("⌘= / ⌘− zoom both"),
                    )
                    .child(
                        Button::new("reset-sizes")
                            .label("Reset")
                            .ghost()
                            .small()
                            .disabled(s == Settings::default())
                            .on_click(|_, _, cx| {
                                settings::update(cx, |s| *s = Settings::default())
                            }),
                    )
                    .child(
                        Button::new("close-settings")
                            .label("Done")
                            .primary()
                            .small()
                            .on_click(|_, w, cx| w.close_dialog(cx)),
                    ),
            )
    }
}
