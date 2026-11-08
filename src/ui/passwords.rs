//! Passwords & Keychain (design.md 24.4 / 24.4b): before Tabula first
//! touches the keychain, say what it stores there and what the system prompt
//! will ask, so that prompt never arrives without context. Continue does a
//! harmless keychain read so the prompt shows right then, and the outcome
//! (allowed / refused) replaces the illustration. Shown once: as onboarding's
//! fourth step, or as a dialog the first time a password is involved later.
//! "Don't save passwords" turns the keychain off instead.

use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::{
    AnyElement, App, AppContext as _, BoxShadow, Context, FontWeight, Hsla, IntoElement,
    ParentElement as _, Render, Styled as _, Window, div, hsla, point, prelude::*, px,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::{Icon, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit_assets::IconName;

use super::dialog;
use super::logo::logo;
use super::theme::{Tokens, caps, dp, keychain_name, tokens, tokens_for};
use crate::runtime;
use crate::settings;
use crate::store;

/// What to do once the user is past the step (whichever way).
pub type Then = Rc<dyn Fn(&mut Window, &mut App)>;

/// How long the "allowed" card shows before moving on by itself.
const AUTO_CONTINUE: Duration = Duration::from_millis(1200);

/// Runs `then` right away if the step was already shown (or passwords are
/// off); otherwise shows it first, as a dialog.
pub fn gate(window: &mut Window, cx: &mut App, then: Then) {
    let s = settings::get(cx);
    if s.keychain_explained || !s.save_passwords {
        then(window, cx);
        return;
    }
    open_dialog(then, window, cx);
}

fn open_dialog(then: Then, window: &mut Window, cx: &mut App) {
    let view = cx.new(|_| PasswordsStep::new(then, true));
    let width = dialog::width(940., cx);
    window.open_dialog(cx, move |d, _, cx| {
        dialog::shell(d, width, cx).child(view.clone())
    });
}

/// For screenshots: the step, whatever was seen before.
#[cfg(test)]
pub fn gate_always(window: &mut Window, cx: &mut App) {
    open_dialog(Rc::new(|_, _| {}), window, cx);
}

/// What the keychain said when asked.
#[derive(Clone, Debug, PartialEq)]
pub enum Outcome {
    NotAsked,
    Checking,
    Allowed(Instant),
    Refused(String),
}

pub struct PasswordsStep {
    then: Then,
    /// Shown as a dialog (closes itself) rather than inside onboarding.
    in_dialog: bool,
    outcome: Outcome,
    done: bool,
}

impl PasswordsStep {
    pub fn new(then: Then, in_dialog: bool) -> Self {
        Self {
            then,
            in_dialog,
            outcome: Outcome::NotAsked,
            done: false,
        }
    }

    #[cfg(test)]
    pub fn outcome(&self) -> &Outcome {
        &self.outcome
    }

    /// For screenshots: the outcome cards without asking the keychain.
    #[cfg(test)]
    pub fn show_outcome(&mut self, outcome: Outcome, cx: &mut Context<Self>) {
        self.outcome = outcome;
        cx.notify();
    }

    /// ↵: ask the keychain, or move on once it answered.
    pub fn primary(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.outcome {
            Outcome::NotAsked => self.ask(window, cx),
            Outcome::Allowed(_) => self.finish(true, window, cx),
            Outcome::Refused(_) => self.finish(false, window, cx),
            Outcome::Checking => {}
        }
    }

    /// Continue / Try again: a harmless keychain read, which is what makes
    /// macOS show its prompt now.
    fn ask(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.outcome = Outcome::Checking;
        cx.notify();
        let task =
            runtime::spawn(async { tokio::task::spawn_blocking(store::probe_keychain).await? });
        cx.spawn_in(window, async move |this, cx| {
            let answer = task.await;
            this.update_in(cx, |step, window, cx| {
                match answer {
                    Ok(()) => {
                        step.outcome = Outcome::Allowed(Instant::now());
                        // Moves on by itself after a moment (↵ sooner).
                        cx.spawn_in(window, async move |this, cx| {
                            cx.background_executor().timer(AUTO_CONTINUE).await;
                            this.update_in(cx, |step, window, cx| {
                                if matches!(step.outcome, Outcome::Allowed(_)) {
                                    step.finish(true, window, cx);
                                }
                            })
                            .ok();
                        })
                        .detach();
                        // The progress bar fills meanwhile.
                        cx.spawn(async move |this, cx| {
                            for _ in 0..12 {
                                cx.background_executor()
                                    .timer(Duration::from_millis(100))
                                    .await;
                                if this.update(cx, |_, cx| cx.notify()).is_err() {
                                    break;
                                }
                            }
                        })
                        .detach();
                    }
                    Err(error) => step.outcome = Outcome::Refused(format!("{error:#}")),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Records the choice and hands over (once).
    pub fn finish(&mut self, save: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.done {
            return;
        }
        self.done = true;
        settings::update(cx, |s| {
            s.keychain_explained = true;
            s.save_passwords = save;
        });
        if self.in_dialog {
            window.close_dialog(cx);
        }
        let then = self.then.clone();
        // After this step is gone: what follows may open the system prompt
        // or another dialog.
        window.defer(cx, move |window, cx| then(window, cx));
    }

    fn render_explainer(&self, cx: &mut Context<Self>) -> AnyElement {
        let t = tokens(cx);
        let mac = cfg!(target_os = "macos");
        let point_row = |icon: IconName, title: &str, text: String| {
            h_flex()
                .gap(dp(12.))
                .items_start()
                .child(
                    div()
                        .flex()
                        .flex_none()
                        .items_center()
                        .justify_center()
                        .size(dp(28.))
                        .rounded(dp(7.))
                        .bg(t.hover)
                        .child(Icon::new(icon).small().text_color(t.text_secondary)),
                )
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .gap(dp(2.))
                        .child(
                            div()
                                .text_size(dp(13.))
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(t.text)
                                .child(title.to_string()),
                        )
                        .child(
                            div()
                                .text_size(dp(12.))
                                .text_color(t.text_muted)
                                .child(text),
                        ),
                )
        };
        let checking = self.outcome == Outcome::Checking;
        let asked = !matches!(self.outcome, Outcome::NotAsked | Outcome::Checking);
        v_flex()
            .flex_1()
            .min_w_0()
            .gap(dp(18.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_center()
                    .size(dp(40.))
                    .rounded(dp(10.))
                    .bg(t.success.opacity(0.14))
                    .child(Icon::new(IconName::KeyRound).text_color(t.success)),
            )
            .child(
                div()
                    .text_size(dp(26.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(t.text)
                    .child(format!("Your passwords stay in {}", keychain_name())),
            )
            .child(
                div()
                    .text_size(dp(13.5))
                    .text_color(t.text_secondary)
                    .child(format!(
                        "Tabula needs a safe place to keep database and SSH passwords. Instead of writing them to a file, it uses {}.",
                        keychain_name()
                    )),
            )
            .child(point_row(
                IconName::Lock,
                &format!("Only in {}", keychain_name()),
                "Never in settings files, logs or command lines.".into(),
            ))
            .child(point_row(
                IconName::KeyRound,
                "One item per connection",
                "Named \"tabula\": you can see and remove them in your keychain app.".into(),
            ))
            .child(point_row(
                IconName::ShieldCheck,
                "Asked once",
                if mac {
                    "macOS will ask for permission next. Choose Always Allow so it doesn't ask every time you connect.".into()
                } else {
                    "Your system may ask whether Tabula may use the keyring.".into()
                },
            ))
            .when(!asked, |column| {
                column
                    .child(
                        h_flex()
                            .gap(dp(10.))
                            .pt(dp(4.))
                            .child(
                                div()
                                    .debug_selector(|| "passwords-continue".into())
                                    .rounded(dp(8.))
                                    .shadow(vec![BoxShadow {
                                        color: t.accent.opacity(0.35),
                                        offset: point(px(0.), px(4.)),
                                        blur_radius: px(18.),
                                        spread_radius: px(0.),
            inset: false,
                                    }])
                                    .child(
                                        Button::new("passwords-continue")
                                            .cursor_pointer()
                                            .label(if mac {
                                                "Continue — macOS will ask next"
                                            } else {
                                                "Continue"
                                            })
                                            .primary()
                                            .h(dp(40.))
                                            .loading(checking)
                                            .on_click(cx.listener(|step, _, window, cx| {
                                                step.ask(window, cx)
                                            })),
                                    ),
                            )
                            .child(
                                div().debug_selector(|| "no-passwords".into()).child(
                                    Button::new("no-passwords")
                                        .cursor_pointer()
                                        .label("Don't save passwords")
                                        .outline()
                                        .h(dp(40.))
                                        .on_click(cx.listener(|step, _, window, cx| {
                                            step.finish(false, window, cx)
                                        })),
                                ),
                            ),
                    )
                    .child(
                        div()
                            .text_size(dp(11.5))
                            .text_color(t.text_muted)
                            .child("Without the keychain, Tabula asks for the password each time you connect and stores nothing."),
                    )
            })
            .into_any_element()
    }

    /// Right column: the system prompt to expect, or what it answered.
    fn render_side(&self, cx: &mut Context<Self>) -> AnyElement {
        let t = tokens(cx);
        let card = v_flex()
            .w(dp(380.))
            .flex_none()
            .gap(dp(14.))
            .p(dp(18.))
            .rounded(dp(14.))
            .border_1()
            .border_color(t.border)
            .bg(t.sidebar);
        match &self.outcome {
            Outcome::Allowed(since) => {
                let progress = (since.elapsed().as_secs_f32() / AUTO_CONTINUE.as_secs_f32()).min(1.);
                card.child(outcome_head(
                    IconName::Check,
                    t.success,
                    &format!("Passwords will be saved in {}", keychain_name()),
                    "Tabula can now remember passwords for your connections. Continuing in a moment…",
                    &t,
                ))
                .child(
                    div()
                        .ml(dp(48.))
                        .h(dp(4.))
                        .rounded_full()
                        .bg(t.hover)
                        .child(div().h_full().w(gpui::relative(progress)).rounded_full().bg(t.success)),
                )
                .child(
                    div()
                        .text_size(dp(11.5))
                        .text_color(t.text_muted)
                        .child("Continues by itself; ↵ continues now."),
                )
                .into_any_element()
            }
            Outcome::Refused(_) => {
                let numbered = |index: usize, text: &str| {
                    h_flex()
                        .gap(dp(8.))
                        .text_size(dp(12.))
                        .text_color(t.text_secondary)
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .justify_center()
                                .size(dp(16.))
                                .rounded_full()
                                .bg(t.hover)
                                .text_size(dp(10.))
                                .child(index.to_string()),
                        )
                        .child(text.to_string())
                };
                let mac = cfg!(target_os = "macos");
                card.child(outcome_head(
                    IconName::TriangleAlert,
                    t.warning,
                    "Passwords won't be saved",
                    "That's fine. Tabula will ask for a password when you connect, and keep nothing.",
                    &t,
                ))
                .when(mac, |card| {
                    card.child(
                        v_flex()
                            .gap(dp(8.))
                            .p(dp(12.))
                            .rounded(dp(10.))
                            .border_1()
                            .border_color(t.border)
                            .bg(t.canvas)
                            .child(
                                div()
                                    .text_size(dp(12.))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(t.text)
                                    .child("Changed your mind?"),
                            )
                            .child(numbered(1, "Open Keychain Access"))
                            .child(numbered(2, "Search for \"tabula\""))
                            .child(numbered(3, "Open an item → Access Control"))
                            .child(numbered(4, "Add Tabula, or allow all apps")),
                    )
                })
                .child(
                    h_flex()
                        .gap(dp(8.))
                        .child(
                            div().debug_selector(|| "keychain-try-again".into()).child(
                                Button::new("keychain-try-again")
                                    .cursor_pointer()
                                    .label("Try again")
                                    .outline()
                                    .on_click(cx.listener(|step, _, window, cx| step.ask(window, cx))),
                            ),
                        )
                        .child(
                            div().debug_selector(|| "keychain-continue-anyway".into()).child(
                                Button::new("keychain-continue-anyway")
                                    .cursor_pointer()
                                    .label("Continue anyway")
                                    .primary()
                                    .on_click(cx.listener(|step, _, window, cx| {
                                        step.finish(false, window, cx)
                                    })),
                            ),
                        ),
                )
                .into_any_element()
            }
            Outcome::NotAsked | Outcome::Checking if cfg!(target_os = "macos") => card
                .child(caps("What macOS will show", cx))
                .child(mac_prompt(cx))
                .child(
                    h_flex()
                        .gap(dp(10.))
                        .p(dp(10.))
                        .rounded(dp(8.))
                        .border_1()
                        .border_color(t.accent.opacity(0.4))
                        .bg(t.accent.opacity(0.10))
                        .child(Icon::new(IconName::Check).small().text_color(t.accent))
                        .child(
                            v_flex()
                                .gap(dp(1.))
                                .child(
                                    div()
                                        .text_size(dp(12.5))
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .text_color(t.text)
                                        .child("Choose \"Always Allow\""),
                                )
                                .child(
                                    div()
                                        .text_size(dp(11.5))
                                        .text_color(t.text_secondary)
                                        .child("Then macOS won't ask again each time you connect."),
                                ),
                        ),
                )
                .child(
                    div()
                        .text_size(dp(11.5))
                        .text_color(t.text_muted)
                        .child("macOS may ask again after Tabula updates, until releases are signed."),
                )
                .into_any_element(),
            Outcome::NotAsked | Outcome::Checking => card
                .child(caps("Where they go", cx))
                .child(
                    div()
                        .text_size(dp(12.5))
                        .text_color(t.text_secondary)
                        .child(if cfg!(windows) {
                            "Windows Credential Manager keeps them for your account. There's no prompt: Windows lets Tabula use it."
                        } else {
                            "The Secret Service (GNOME Keyring or KWallet) keeps them for your login. Your desktop may ask to unlock it."
                        }),
                )
                .into_any_element(),
        }
    }
}

/// The icon, title and line at the top of an outcome card.
fn outcome_head(icon: IconName, colour: Hsla, title: &str, text: &str, t: &Tokens) -> AnyElement {
    h_flex()
        .gap(dp(12.))
        .items_start()
        .child(
            div()
                .flex()
                .flex_none()
                .items_center()
                .justify_center()
                .size(dp(36.))
                .rounded_full()
                .bg(colour.opacity(0.16))
                .child(Icon::new(icon).text_color(colour)),
        )
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .gap(dp(4.))
                .child(
                    div()
                        .text_size(dp(14.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(t.text)
                        .child(title.to_string()),
                )
                .child(
                    div()
                        .text_size(dp(12.))
                        .text_color(t.text_secondary)
                        .child(text.to_string()),
                ),
        )
        .into_any_element()
}

/// A drawing of the macOS keychain prompt (not a screenshot), in the
/// light system look whatever Tabula's theme, with Always Allow ringed.
fn mac_prompt(cx: &App) -> AnyElement {
    let t = tokens(cx);
    let system = tokens_for(false);
    let button = |label: &str, style: u8| {
        div()
            .flex_1()
            .flex()
            .justify_center()
            .py(dp(5.))
            .rounded(dp(6.))
            .text_size(dp(11.))
            .font_weight(FontWeight::MEDIUM)
            .map(|d| match style {
                // Always Allow: ringed in the accent.
                0 => d
                    .bg(system.canvas)
                    .border_2()
                    .border_color(t.accent)
                    .text_color(system.text),
                // Allow: the system's default button.
                2 => d.bg(system.info).text_color(hsla(0., 0., 1., 1.)),
                _ => d
                    .bg(system.canvas)
                    .border_1()
                    .border_color(system.border)
                    .text_color(system.text),
            })
            .child(label.to_string())
    };
    v_flex()
        .items_center()
        .gap(dp(8.))
        .p(dp(16.))
        .rounded(dp(12.))
        .bg(system.surface)
        .border_1()
        .border_color(system.border)
        .shadow(vec![BoxShadow {
            color: hsla(0., 0., 0., 0.3),
            offset: point(px(0.), px(6.)),
            blur_radius: px(18.),
            spread_radius: px(0.),
            inset: false,
        }])
        .child(
            div()
                .relative()
                .size(dp(52.))
                .rounded(dp(12.))
                .bg(system.hover)
                .flex()
                .items_center()
                .justify_center()
                .child(Icon::new(IconName::Lock).text_color(system.text_muted))
                .child(div().absolute().right(dp(-6.)).bottom(dp(-6.)).child(logo(dialog::width(22., cx), false))),
        )
        .child(
            div()
                .text_center()
                .text_size(dp(11.5))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(system.text)
                .child("Tabula wants to use your confidential information stored in \"tabula\" in your keychain."),
        )
        .child(
            div()
                .text_size(dp(10.5))
                .text_color(system.text_secondary)
                .child("To allow this, enter the \"login\" keychain password."),
        )
        .child(
            div()
                .w_full()
                .px(dp(8.))
                .py(dp(4.))
                .rounded(dp(5.))
                .border_1()
                .border_color(system.border)
                .bg(system.canvas)
                .text_size(dp(10.5))
                .text_color(system.text_faint)
                .child("Password"),
        )
        .child(
            h_flex()
                .w_full()
                .gap(dp(6.))
                .child(button("Always Allow", 0))
                .child(button("Deny", 1))
                .child(button("Allow", 2)),
        )
        .into_any_element()
}

impl Render for PasswordsStep {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        h_flex()
            .w_full()
            .gap(dp(40.))
            .items_center()
            .child(self.render_explainer(cx))
            .child(self.render_side(cx))
    }
}
