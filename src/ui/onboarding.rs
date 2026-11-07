//! First-run onboarding (design.md 24.1–24.5): Welcome, Look & feel,
//! Connections, You're set. Shown in place of the home screen on a fresh
//! install; every step can be skipped and nothing in it is required. It
//! never touches the keychain itself: the connection choices go through
//! Home, which shows the "Passwords" step when one is involved.

use std::rc::Rc;

use gpui::{
    AnyElement, App, Context, Div, Entity, FocusHandle, Focusable, FontWeight, Hsla, IntoElement,
    KeyDownEvent, ParentElement as _, Render, SharedString, Styled as _, WeakEntity, Window, div,
    prelude::*, relative,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::{ActiveTheme as _, Icon, Sizable as _, h_flex, v_flex};
use gpui_kit_assets::IconName;

use super::app::AppView;
use super::dialog;
use super::home::Home;
use super::settings_view::set_appearance;
use super::theme::{Tokens, caps, dp, kbd, tokens, tokens_for};
use crate::settings::{self, Appearance};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Step {
    Welcome,
    Look,
    Connections,
    Done,
}

const STEPS: [Step; 4] = [Step::Welcome, Step::Look, Step::Connections, Step::Done];

/// What the Connections step started, for the checklist on the last one.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Picked {
    Nothing,
    Import,
    NewConnection,
    SqliteFile,
    Sample,
}

/// Text sizes on the Look & feel step: Compact / Default / Large.
const SIZES: [(f32, &str); 3] = [(12., "Compact"), (13., "Default"), (15., "Large")];

pub struct Onboarding {
    app: WeakEntity<AppView>,
    home: Entity<Home>,
    step: Step,
    picked: Picked,
    /// "Coming from TablePlus?" on Welcome: Import is singled out.
    import_first: bool,
    focus: FocusHandle,
}

impl Onboarding {
    pub fn new(app: WeakEntity<AppView>, home: Entity<Home>, cx: &mut Context<Self>) -> Self {
        Self {
            app,
            home,
            step: Step::Welcome,
            picked: Picked::Nothing,
            import_first: false,
            focus: cx.focus_handle(),
        }
    }

    #[cfg(test)]
    pub fn step(&self) -> Step {
        self.step
    }

    /// For screenshots.
    #[cfg(test)]
    pub fn show_step(&mut self, step: Step, cx: &mut Context<Self>) {
        self.go(step, cx);
    }

    fn go(&mut self, step: Step, cx: &mut Context<Self>) {
        self.step = step;
        cx.notify();
    }

    fn next(&mut self, cx: &mut Context<Self>) {
        let at = STEPS.iter().position(|s| *s == self.step).unwrap_or(0);
        match STEPS.get(at + 1) {
            Some(step) => self.go(*step, cx),
            None => self.finish(cx),
        }
    }

    fn back(&mut self, cx: &mut Context<Self>) {
        let at = STEPS.iter().position(|s| *s == self.step).unwrap_or(0);
        self.go(STEPS[at.saturating_sub(1)], cx);
    }

    /// Done or skipped: never shown again; the app takes over.
    pub fn finish(&mut self, cx: &mut Context<Self>) {
        settings::update(cx, |s| s.onboarded = true);
        let app = self.app.clone();
        // AppView drops this view: not while it's being updated.
        cx.defer(move |cx| {
            app.update(cx, |app, cx| app.end_onboarding(cx)).ok();
        });
    }

    /// One of the Connections step's cards: start it, and move on to the
    /// last step, which waits behind whatever it opened.
    fn pick(&mut self, picked: Picked, window: &mut Window, cx: &mut Context<Self>) {
        self.picked = picked;
        self.go(Step::Done, cx);
        let home = self.home.clone();
        window.defer(cx, move |window, cx| {
            home.update(cx, |home, cx| match picked {
                Picked::Import => home.import_any(window, cx),
                Picked::NewConnection => home.open_form(None, window, cx),
                Picked::SqliteFile => home.open_sqlite_file(window, cx),
                Picked::Sample => home.open_sample(window, cx),
                Picked::Nothing => {}
            });
        });
    }

    fn on_key(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let keys = &event.keystroke;
        if keys.modifiers.modified() {
            return;
        }
        match (keys.key.as_str(), self.step) {
            ("enter", Step::Connections) => {}
            ("enter", _) => {
                cx.stop_propagation();
                self.next(cx);
            }
            ("escape", Step::Look | Step::Connections) => {
                cx.stop_propagation();
                self.back(cx);
            }
            _ => {}
        }
    }

    fn render_welcome(&self, cx: &mut Context<Self>) -> AnyElement {
        let t = tokens(cx);
        let feature = |icon: IconName, title: &str, text: &str| {
            v_flex()
                .flex_1()
                .min_w_0()
                .gap(dp(8.))
                .p(dp(16.))
                .rounded(dp(12.))
                .border_1()
                .border_color(t.border)
                .bg(t.sidebar)
                .child(Icon::new(icon).text_color(t.accent))
                .child(
                    div()
                        .text_size(dp(13.5))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(t.text)
                        .child(title.to_string()),
                )
                .child(
                    div()
                        .text_size(dp(12.))
                        .text_color(t.text_muted)
                        .child(text.to_string()),
                )
        };
        v_flex()
            .items_center()
            .gap(dp(22.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_center()
                    .size(dp(64.))
                    .rounded(dp(16.))
                    .bg(t.accent.opacity(0.15))
                    .child(Icon::new(IconName::Database).size(dp(30.)).text_color(t.accent)),
            )
            .child(
                v_flex()
                    .items_center()
                    .gap(dp(8.))
                    .child(
                        div()
                            .text_size(dp(28.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(t.text)
                            .child("Welcome to Tabula"),
                    )
                    .child(
                        div()
                            .text_size(dp(14.))
                            .text_color(t.text_secondary)
                            .child("A fast, native client for PostgreSQL, MySQL, SQLite, Redis and MongoDB."),
                    ),
            )
            .child(
                h_flex()
                    .w_full()
                    .gap(dp(12.))
                    .items_start()
                    .child(feature(
                        IconName::Zap,
                        "Fast and native",
                        "Written in Rust and drawn on the GPU: big tables scroll smoothly.",
                    ))
                    .child(feature(
                        IconName::ShieldCheck,
                        "Safe by default",
                        "Edits wait until you commit, and production shows the SQL first.",
                    ))
                    .child(feature(
                        IconName::Layers,
                        "Every database",
                        "Postgres, MySQL, SQLite, Redis and MongoDB in one window.",
                    )),
            )
            .child(
                h_flex()
                    .gap(dp(6.))
                    .text_size(dp(12.))
                    .text_color(t.text_muted)
                    .child(Icon::new(IconName::Lock).xsmall())
                    .child("No account, no tracking. Your connections and passwords stay on this computer."),
            )
            .child(
                v_flex()
                    .items_center()
                    .gap(dp(10.))
                    .child(
                        div().debug_selector(|| "onboarding-start".into()).child(
                            Button::new("get-started")
                                .cursor_pointer()
                                .label("Get started ↵")
                                .primary()
                                .on_click(cx.listener(|this, _, _, cx| this.next(cx))),
                        ),
                    )
                    .child(
                        Button::new("from-tableplus")
                            .cursor_pointer()
                            .label("Coming from TablePlus? Import your connections")
                            .ghost()
                            .small()
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.import_first = true;
                                this.go(Step::Connections, cx);
                            })),
                    ),
            )
            .into_any_element()
    }

    fn render_look(&self, cx: &mut Context<Self>) -> AnyElement {
        let t = tokens(cx);
        let s = settings::get(cx);
        let theme_card = |index: usize, label: &str, choice: Appearance| {
            let selected = s.appearance == choice;
            let preview = match choice {
                Appearance::System => h_flex()
                    .size_full()
                    .child(mini_window(tokens_for(true)).w(relative(0.5)))
                    .child(mini_window(tokens_for(false)).w(relative(0.5))),
                Appearance::Dark => h_flex().size_full().child(mini_window(tokens_for(true))),
                Appearance::Light => h_flex().size_full().child(mini_window(tokens_for(false))),
            };
            v_flex()
                .id(("theme-card", index))
                .debug_selector(move || format!("theme-card-{index}"))
                .flex_1()
                .gap(dp(8.))
                .p(dp(8.))
                .rounded(dp(12.))
                .border_2()
                .border_color(if selected { t.accent } else { t.border })
                .cursor_pointer()
                .hover(|style| style.bg(t.hover))
                .child(
                    div()
                        .h(dp(96.))
                        .rounded(dp(8.))
                        .overflow_hidden()
                        .border_1()
                        .border_color(t.border)
                        .child(preview),
                )
                .child(
                    div()
                        .text_center()
                        .text_size(dp(12.5))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(if selected { t.text } else { t.text_secondary })
                        .child(label.to_string()),
                )
                .on_click(move |_, window, cx| set_appearance(index, window, cx))
        };
        let size_index = SIZES
            .iter()
            .position(|(size, _)| *size == s.ui_font_size)
            .unwrap_or(1);
        v_flex()
            .gap(dp(22.))
            .child(step_title(
                "Look & feel",
                "Change these any time in Settings (⌘,).",
                cx,
            ))
            .child(caps("Theme", cx))
            .child(
                h_flex()
                    .gap(dp(12.))
                    .child(theme_card(0, "System", Appearance::System))
                    .child(theme_card(1, "Light", Appearance::Light))
                    .child(theme_card(2, "Dark", Appearance::Dark)),
            )
            .child(caps("Text size", cx))
            .child(
                dialog::segmented(
                    "text-size",
                    &SIZES.map(|(_, label)| label),
                    size_index,
                    Rc::new(|index, _, cx| {
                        let size = SIZES[index].0;
                        settings::update(cx, |s| s.ui_font_size = size);
                    }),
                    cx,
                )
                .w(dp(360.)),
            )
            .child(
                h_flex()
                    .gap(dp(16.))
                    .px(dp(14.))
                    .h(dp(36.))
                    .rounded(dp(8.))
                    .border_1()
                    .border_color(t.border)
                    .bg(t.canvas)
                    .font_family(gpui_mono(cx))
                    .text_size(dp(12.))
                    .text_color(t.text)
                    .child("10427")
                    .child("Priya Raman")
                    .child(div().text_color(t.success).child("paid"))
                    .child(div().flex_1())
                    .child("1,284.50"),
            )
            .child(nav(true, "Continue ↵", cx))
            .into_any_element()
    }

    fn render_connections(&self, cx: &mut Context<Self>) -> AnyElement {
        let t = tokens(cx);
        let card = |id: &'static str,
                    icon: IconName,
                    title: &str,
                    text: &str,
                    picked: Picked,
                    stand_out: bool,
                    cx: &mut Context<Self>| {
            h_flex()
                .id(id)
                .debug_selector(move || id.to_string())
                .gap(dp(14.))
                .p(dp(16.))
                .rounded(dp(12.))
                .border_1()
                .border_color(if stand_out { t.accent } else { t.border })
                .bg(if stand_out {
                    t.accent.opacity(0.08)
                } else {
                    t.sidebar
                })
                .cursor_pointer()
                .hover(|style| style.bg(t.hover))
                .child(
                    div()
                        .flex()
                        .flex_none()
                        .items_center()
                        .justify_center()
                        .size(dp(40.))
                        .rounded(dp(10.))
                        .bg(t.accent.opacity(0.14))
                        .child(Icon::new(icon).text_color(t.accent)),
                )
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .gap(dp(3.))
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
                                .text_color(t.text_muted)
                                .child(text.to_string()),
                        ),
                )
                .child(Icon::new(IconName::ChevronRight).text_color(t.text_faint))
                .on_click(cx.listener(move |this, _, window, cx| this.pick(picked, window, cx)))
        };
        v_flex()
            .gap(dp(14.))
            .child(step_title(
                "Your connections",
                "Pick one to start with; you can add more any time.",
                cx,
            ))
            .child(card(
                "pick-import",
                IconName::Download,
                "Import from TablePlus",
                "Your TablePlus connections on this Mac, or an export file.",
                Picked::Import,
                self.import_first,
                cx,
            ))
            .child(card(
                "pick-new",
                IconName::Plus,
                "New connection",
                "PostgreSQL, MySQL, SQLite, Redis or MongoDB; paste a URL to fill it in.",
                Picked::NewConnection,
                false,
                cx,
            ))
            .child(card(
                "pick-sqlite",
                IconName::FileSearch,
                "Open a SQLite file",
                "Any .sqlite / .db file on this computer.",
                Picked::SqliteFile,
                false,
                cx,
            ))
            .child(card(
                "pick-sample",
                IconName::Sparkles,
                "Try the sample database",
                "A small shop to explore: no password, no network.",
                Picked::Sample,
                false,
                cx,
            ))
            .child(
                h_flex()
                    .pt(dp(8.))
                    .child(
                        Button::new("onboarding-back")
                            .cursor_pointer()
                            .label("Back")
                            .ghost()
                            .on_click(cx.listener(|this, _, _, cx| this.back(cx))),
                    )
                    .child(div().flex_1())
                    .child(
                        div().debug_selector(|| "onboarding-later".into()).child(
                            Button::new("later")
                                .cursor_pointer()
                                .label("I'll do this later")
                                .ghost()
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.pick(Picked::Nothing, window, cx)
                                })),
                        ),
                    ),
            )
            .into_any_element()
    }

    fn render_done(&self, cx: &mut Context<Self>) -> AnyElement {
        let t = tokens(cx);
        let s = settings::get(cx);
        let theme = match s.appearance {
            Appearance::System => "System",
            Appearance::Light => "Light",
            Appearance::Dark => "Dark",
        };
        let saved = self.home.read(cx).connections.len();
        let connection_line = match (self.picked, saved) {
            (Picked::Sample, _) => "The sample database is open".to_string(),
            (_, 0) => "No connection yet: add one any time from Connections".to_string(),
            (_, 1) => "1 connection saved".to_string(),
            (_, count) => format!("{count} connections saved"),
        };
        let check = |done: bool, text: String| {
            h_flex()
                .gap(dp(10.))
                .text_size(dp(13.))
                .text_color(t.text)
                .child(
                    Icon::new(if done {
                        IconName::CircleCheck
                    } else {
                        IconName::Circle
                    })
                    .small()
                    .text_color(if done { t.success } else { t.text_faint }),
                )
                .child(text)
        };
        let shortcut = |keys: &str, what: &str| {
            h_flex()
                .gap(dp(10.))
                .text_size(dp(12.5))
                .text_color(t.text_secondary)
                .child(div().w(dp(48.)).child(kbd(keys.to_string(), cx)))
                .child(what.to_string())
        };
        v_flex()
            .gap(dp(20.))
            .child(step_title("You're set", "Here's what's ready.", cx))
            .child(
                v_flex()
                    .gap(dp(10.))
                    .child(check(true, format!("Theme: {theme}")))
                    .child(check(true, format!("Text size: {} px", s.ui_font_size)))
                    .child(check(
                        saved > 0 || self.picked == Picked::Sample,
                        connection_line,
                    ))
                    .child(check(
                        true,
                        if s.save_passwords {
                            "Passwords go to the system keychain, never to files".to_string()
                        } else {
                            "Passwords aren't saved: Tabula asks when you connect".to_string()
                        },
                    )),
            )
            .child(caps("Worth knowing", cx))
            .child(
                v_flex()
                    .gap(dp(8.))
                    .child(shortcut("⌘K", "Switch database"))
                    .child(shortcut("⌘P", "Go to a table, query or command"))
                    .child(shortcut("⌘↵", "Run the statement at the cursor"))
                    .child(shortcut("⌘S", "Commit staged changes")),
            )
            .child(
                h_flex().justify_end().child(
                    div().debug_selector(|| "onboarding-finish".into()).child(
                        Button::new("open-tabula")
                            .cursor_pointer()
                            .label("Open Tabula ↵")
                            .primary()
                            .on_click(cx.listener(|this, _, _, cx| this.finish(cx))),
                    ),
                ),
            )
            .into_any_element()
    }
}

/// The monospace family, for the text-size preview row.
fn gpui_mono(cx: &App) -> SharedString {
    cx.theme().mono_font_family.clone()
}

/// A step's heading and a muted line.
fn step_title(title: &str, sub: &str, cx: &App) -> AnyElement {
    let t = tokens(cx);
    v_flex()
        .gap(dp(6.))
        .child(
            div()
                .text_size(dp(22.))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(t.text)
                .child(title.to_string()),
        )
        .child(
            div()
                .text_size(dp(13.))
                .text_color(t.text_muted)
                .child(sub.to_string()),
        )
        .into_any_element()
}

/// Back / Continue under a step.
fn nav(back: bool, next: &str, cx: &mut Context<Onboarding>) -> AnyElement {
    h_flex()
        .pt(dp(8.))
        .when(back, |row| {
            row.child(
                Button::new("onboarding-back")
                    .cursor_pointer()
                    .label("Back")
                    .ghost()
                    .on_click(cx.listener(|this, _, _, cx| this.back(cx))),
            )
        })
        .child(div().flex_1())
        .child(
            div().debug_selector(|| "onboarding-next".into()).child(
                Button::new("onboarding-next")
                    .cursor_pointer()
                    .label(next.to_string())
                    .primary()
                    .on_click(cx.listener(|this, _, _, cx| this.next(cx))),
            ),
        )
        .into_any_element()
}

/// A tiny window drawn in one mode's colours, for the theme cards.
fn mini_window(t: Tokens) -> Div {
    let bar =
        |color: Hsla, width: f32| div().h(dp(4.)).w(relative(width)).rounded(dp(2.)).bg(color);
    h_flex()
        .size_full()
        .bg(t.canvas)
        .child(div().w(dp(10.)).h_full().bg(t.rail))
        .child(
            v_flex()
                .w(dp(36.))
                .h_full()
                .p(dp(5.))
                .gap(dp(4.))
                .bg(t.sidebar)
                .child(bar(t.text_muted, 0.8))
                .child(bar(t.text_faint, 0.6))
                .child(bar(t.text_faint, 0.7)),
        )
        .child(
            v_flex()
                .flex_1()
                .p(dp(6.))
                .gap(dp(5.))
                .child(bar(t.accent, 0.3))
                .child(bar(t.text_muted, 0.9))
                .child(bar(t.text_faint, 0.8))
                .child(bar(t.text_faint, 0.85)),
        )
}

impl Focusable for Onboarding {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for Onboarding {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = tokens(cx);
        // ↵ / Esc reach the steps with nothing else focused.
        if window.focused(cx).is_none() {
            self.focus.focus(window, cx);
        }
        let body = match self.step {
            Step::Welcome => self.render_welcome(cx),
            Step::Look => self.render_look(cx),
            Step::Connections => self.render_connections(cx),
            Step::Done => self.render_done(cx),
        };
        let at = STEPS.iter().position(|s| *s == self.step).unwrap_or(0);
        div()
            .id("onboarding")
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key))
            .size_full()
            .overflow_y_scroll()
            .bg(t.canvas)
            .child(
                v_flex()
                    .w_full()
                    .max_w(dp(720.))
                    .mx_auto()
                    .px(dp(32.))
                    .py(dp(40.))
                    .gap(dp(24.))
                    .child(
                        h_flex()
                            .child(h_flex().gap(dp(6.)).children(STEPS.iter().enumerate().map(
                                |(index, _)| {
                                    div()
                                        .h(dp(4.))
                                        .w(dp(if index == at { 22. } else { 8. }))
                                        .rounded_full()
                                        .bg(if index <= at { t.accent } else { t.hover })
                                },
                            )))
                            .child(div().flex_1())
                            .when(self.step != Step::Done, |row| {
                                row.child(
                                    div().debug_selector(|| "onboarding-skip".into()).child(
                                        Button::new("skip-setup")
                                            .cursor_pointer()
                                            .label("Skip setup")
                                            .ghost()
                                            .small()
                                            .on_click(
                                                cx.listener(|this, _, _, cx| this.finish(cx)),
                                            ),
                                    ),
                                )
                            }),
                    )
                    .child(body),
            )
    }
}
