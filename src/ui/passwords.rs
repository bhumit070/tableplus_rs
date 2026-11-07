//! The "Passwords" step (design.md 24.4): before Tabula first touches the
//! keychain, say what it stores there and what the system prompt will ask,
//! so that prompt never arrives without context. Shown once; "Don't save
//! passwords" turns the keychain off instead.

use std::rc::Rc;

use gpui::{
    AnyElement, App, AppContext as _, Context, FontWeight, Hsla, IntoElement, ParentElement as _,
    Render, Styled as _, Window, div, prelude::*,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::{Icon, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit_assets::IconName;

use super::dialog;
use super::theme::{dp, keychain_name, tokens};
use crate::settings;

/// What to do once the user has read the step (whichever button).
pub type Then = Rc<dyn Fn(&mut Window, &mut App)>;

/// Runs `then` right away if the step was already shown (or passwords are
/// off); otherwise shows it first.
pub fn gate(window: &mut Window, cx: &mut App, then: Then) {
    let s = settings::get(cx);
    if s.keychain_explained || !s.save_passwords {
        then(window, cx);
        return;
    }
    let view = cx.new(|_| PasswordsStep { then });
    let w = dialog::width(560., cx);
    window.open_dialog(cx, move |d, _, cx| {
        dialog::shell(d, w, cx).child(view.clone())
    });
}

/// For screenshots: the step, whatever was seen before.
#[cfg(test)]
pub fn gate_always(window: &mut Window, cx: &mut App) {
    let view = cx.new(|_| PasswordsStep {
        then: Rc::new(|_, _| {}),
    });
    let w = dialog::width(560., cx);
    window.open_dialog(cx, move |d, _, cx| {
        dialog::shell(d, w, cx).child(view.clone())
    });
}

pub struct PasswordsStep {
    then: Then,
}

impl PasswordsStep {
    fn finish(&mut self, save: bool, window: &mut Window, cx: &mut Context<Self>) {
        settings::update(cx, |s| {
            s.keychain_explained = true;
            s.save_passwords = save;
        });
        window.close_dialog(cx);
        let then = self.then.clone();
        // After this dialog is gone: what follows may open the system
        // prompt or another dialog.
        window.defer(cx, move |window, cx| then(window, cx));
    }
}

/// A sketch of the macOS keychain prompt with "Always Allow" picked out.
fn mac_prompt(cx: &App) -> AnyElement {
    let t = tokens(cx);
    let button = |label: &str, on: bool, accent: Hsla| {
        div()
            .px(dp(10.))
            .py(dp(3.))
            .rounded(dp(5.))
            .text_size(dp(11.))
            .map(|d| {
                if on {
                    d.border_2()
                        .border_color(accent)
                        .bg(accent.opacity(0.15))
                        .text_color(t.text)
                        .font_weight(FontWeight::SEMIBOLD)
                } else {
                    d.border_1().border_color(t.border).text_color(t.text_muted)
                }
            })
            .child(label.to_string())
    };
    v_flex()
        .gap(dp(10.))
        .p(dp(14.))
        .rounded(dp(10.))
        .border_1()
        .border_color(t.border)
        .bg(t.raised)
        .child(
            h_flex()
                .gap(dp(10.))
                .items_start()
                .child(Icon::new(IconName::KeyRound).text_color(t.warning))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_size(dp(11.5))
                        .text_color(t.text_secondary)
                        .child("“Tabula” wants to use your confidential information stored in “tabula” in your keychain."),
                ),
        )
        .child(
            h_flex()
                .gap(dp(6.))
                .justify_end()
                .child(button("Always Allow", true, t.accent))
                .child(button("Deny", false, t.accent))
                .child(button("Allow", false, t.accent)),
        )
        .into_any_element()
}

impl Render for PasswordsStep {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = tokens(cx);
        let mac = cfg!(target_os = "macos");
        let line = |icon: IconName, text: String| {
            h_flex()
                .gap(dp(10.))
                .items_start()
                .text_size(dp(12.5))
                .text_color(t.text_secondary)
                .child(Icon::new(icon).small().text_color(t.text_muted))
                .child(div().flex_1().min_w_0().child(text))
        };
        v_flex()
            .gap(dp(16.))
            .child(dialog::title(
                format!("Passwords stay in {}", keychain_name()),
                Some("Before Tabula saves or reads one, here's what happens.".into()),
                cx,
            ))
            .child(
                v_flex()
                    .gap(dp(10.))
                    .child(line(
                        IconName::Lock,
                        format!(
                            "Passwords are stored only in {} (service “tabula”, one item per connection), never in Tabula's files or logs.",
                            keychain_name()
                        ),
                    ))
                    .child(line(
                        IconName::MonitorSmartphone,
                        if mac {
                            "macOS will now ask whether Tabula may use them. Choose Always Allow so it doesn't ask at every connect.".into()
                        } else {
                            "Your system may ask whether Tabula may use the keyring.".into()
                        },
                    ))
                    .child(line(
                        IconName::CircleSlash,
                        "If you choose Don't Allow, nothing is saved: Tabula asks for the password when you connect.".into(),
                    )),
            )
            .when(mac, |d| {
                d.child(mac_prompt(cx)).child(
                    div()
                        .text_size(dp(11.5))
                        .text_color(t.text_muted)
                        .child("macOS may ask again after an update, until Tabula's releases are signed."),
                )
            })
            .child(dialog::footer(
                "",
                vec![
                    Button::new("no-passwords")
                        .cursor_pointer()
                        .label("Don't save passwords")
                        .ghost()
                        .on_click(cx.listener(|this, _, window, cx| this.finish(false, window, cx)))
                        .into_any_element(),
                    div()
                        .debug_selector(|| "passwords-continue".into())
                        .child(
                            Button::new("passwords-continue")
                                .cursor_pointer()
                                .label("Continue")
                                .primary()
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.finish(true, window, cx)
                                })),
                        )
                        .into_any_element(),
                ],
                cx,
            ))
    }
}
