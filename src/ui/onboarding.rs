//! First-run onboarding (design.md 24.1–24.5): Welcome, Look & feel,
//! Connections, Passwords & Keychain, You're all set. Shown in place of the
//! whole window on a fresh install (just the traffic lights over a soft
//! glow); every step can be skipped and nothing in it is required. The
//! Passwords step is only part of the flow when the chosen path stores or
//! reads a password and the keychain hasn't been explained yet.

use std::rc::Rc;

use gpui::{
    AnyElement, App, BoxShadow, Context, Div, Entity, FocusHandle, Focusable, FontWeight, Hsla,
    IntoElement, KeyDownEvent, ParentElement as _, Pixels, Render, SharedString, Styled as _,
    WeakEntity, Window, div, point, prelude::*, px, relative,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::{ActiveTheme as _, Icon, Sizable as _, h_flex, v_flex};
use gpui_kit_assets::IconName;

use super::app::AppView;
use super::dialog;
use super::grid::enum_pill;
use super::home::Home;
use super::logo::logo;
use super::passwords::PasswordsStep;
use super::settings_view::set_appearance;
use super::theme::{Tokens, caps, dp, kbd, keychain_name, tokens, tokens_for};
use crate::settings::{self, Appearance};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Step {
    Welcome,
    Look,
    Connections,
    Passwords,
    Done,
}

/// What the Connections step started, for the summary on the last one.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Picked {
    Nothing,
    /// Import from TablePlus: this Mac's list when there is one, else a file.
    Import,
    ImportFile,
    ImportMac,
    NewConnection,
    SqliteFile,
    Sample,
}

impl Picked {
    /// Paths that save or read a password go through the Passwords step.
    fn needs_passwords(self) -> bool {
        matches!(
            self,
            Picked::Import | Picked::ImportFile | Picked::ImportMac | Picked::NewConnection
        )
    }
}

/// Text sizes on the Look & feel step: Compact / Default / Large.
const SIZES: [(f32, &str); 3] = [(12., "Compact"), (13., "Default"), (15., "Large")];

/// The theme cards, in order (← / → move between them).
const THEMES: [(Appearance, &str, &str); 3] = [
    (
        Appearance::System,
        "System",
        "Follows your system's light and dark",
    ),
    (Appearance::Dark, "Dark", "Easy on the eyes"),
    (Appearance::Light, "Light", "Crisp in bright rooms"),
];

pub struct Onboarding {
    app: WeakEntity<AppView>,
    home: Entity<Home>,
    step: Step,
    picked: Picked,
    /// "Coming from TablePlus?" on Welcome: Import is singled out.
    import_first: bool,
    /// The Passwords step while it's showing.
    passwords: Option<Entity<PasswordsStep>>,
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
            passwords: None,
            focus: cx.focus_handle(),
        }
    }

    #[cfg(test)]
    pub fn step(&self) -> Step {
        self.step
    }

    #[cfg(test)]
    pub fn passwords_step(&self) -> Option<Entity<PasswordsStep>> {
        self.passwords.clone()
    }

    /// For screenshots.
    #[cfg(test)]
    pub fn show_step(&mut self, step: Step, window: &mut Window, cx: &mut Context<Self>) {
        if step == Step::Passwords {
            self.picked = Picked::NewConnection;
            self.show_passwords(window, cx);
            return;
        }
        if step == Step::Done && self.picked == Picked::Nothing {
            self.picked = Picked::Sample;
        }
        self.go(step, cx);
    }

    /// Whether the Passwords step is part of this run: it is until a
    /// choice shows it isn't needed.
    fn passwords_in_flow(&self, cx: &App) -> bool {
        let s = settings::get(cx);
        let unexplained = !s.keychain_explained && s.save_passwords;
        match self.picked {
            Picked::Nothing => unexplained || self.step == Step::Passwords,
            picked => picked.needs_passwords() && (unexplained || self.passwords.is_some()),
        }
    }

    /// The progress segments: Welcome, Look & feel, Connections, and
    /// Passwords when it's part of the flow.
    fn segments(&self, cx: &App) -> Vec<(Step, &'static str)> {
        let mut segments = vec![
            (Step::Welcome, "Welcome"),
            (Step::Look, "Look & feel"),
            (Step::Connections, "Connections"),
        ];
        if self.passwords_in_flow(cx) {
            segments.push((Step::Passwords, "Passwords"));
        }
        segments
    }

    fn go(&mut self, step: Step, cx: &mut Context<Self>) {
        self.step = step;
        cx.notify();
    }

    fn next(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.step {
            Step::Welcome => self.go(Step::Look, cx),
            Step::Look => self.go(Step::Connections, cx),
            Step::Connections => {}
            Step::Passwords => {
                if let Some(step) = self.passwords.clone() {
                    step.update(cx, |step, cx| step.primary(window, cx));
                }
            }
            Step::Done => self.finish(window, cx),
        }
    }

    fn back(&mut self, cx: &mut Context<Self>) {
        match self.step {
            Step::Look => self.go(Step::Welcome, cx),
            Step::Connections => self.go(Step::Look, cx),
            Step::Passwords => {
                self.passwords = None;
                self.picked = Picked::Nothing;
                self.go(Step::Connections, cx);
            }
            Step::Welcome | Step::Done => {}
        }
    }

    /// Done or skipped: never shown again; the app takes over. The sample
    /// database opens on its "orders" table.
    pub fn finish(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        settings::update(cx, |s| s.onboarded = true);
        let app = self.app.clone();
        let open_orders = self.picked == Picked::Sample;
        // AppView drops this view: not while it's being updated.
        window.defer(cx, move |window, cx| {
            app.update(cx, |app, cx| {
                app.end_onboarding(cx);
                if open_orders && let Some(session) = app.active_session() {
                    session.update(cx, |session, cx| session.open_named("orders", window, cx));
                }
            })
            .ok();
        });
    }

    /// A Connections card: through the Passwords step when the path needs
    /// it, otherwise straight on (the action opens behind the last step).
    fn pick(&mut self, picked: Picked, window: &mut Window, cx: &mut Context<Self>) {
        self.picked = picked;
        let s = settings::get(cx);
        if picked.needs_passwords() && s.save_passwords && !s.keychain_explained {
            self.show_passwords(window, cx);
        } else {
            self.start_picked(window, cx);
        }
    }

    fn show_passwords(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let onboarding = cx.entity().downgrade();
        let then = Rc::new(move |window: &mut Window, cx: &mut App| {
            onboarding
                .update(cx, |onboarding, cx| onboarding.start_picked(window, cx))
                .ok();
        });
        self.passwords = Some(cx.new(|_| PasswordsStep::new(then, false)));
        self.go(Step::Passwords, cx);
    }

    /// Runs the picked choice behind the last step.
    fn start_picked(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.go(Step::Done, cx);
        let picked = self.picked;
        let home = self.home.clone();
        window.defer(cx, move |window, cx| {
            home.update(cx, |home, cx| match picked {
                Picked::Import => home.import_any(window, cx),
                Picked::ImportFile => home.import_file(window, cx),
                Picked::ImportMac => home.import_tableplus(window, cx),
                Picked::NewConnection => home.open_form(None, window, cx),
                Picked::SqliteFile => home.open_sqlite_file(window, cx),
                Picked::Sample => home.open_sample(window, cx),
                Picked::Nothing => {}
            });
        });
    }

    /// ← / → on Look & feel.
    fn move_theme(&mut self, by: isize, window: &mut Window, cx: &mut Context<Self>) {
        let current = settings::get(cx).appearance;
        let at = THEMES
            .iter()
            .position(|(theme, _, _)| *theme == current)
            .unwrap_or(0) as isize;
        let to = (at + by).clamp(0, THEMES.len() as isize - 1) as usize;
        set_appearance(appearance_index(THEMES[to].0), window, cx);
        cx.notify();
    }

    fn on_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let keys = &event.keystroke;
        if keys.modifiers.platform || keys.modifiers.control || keys.modifiers.alt {
            return;
        }
        match (keys.key.as_str(), self.step) {
            ("enter", Step::Connections) => {}
            ("enter", _) => {
                cx.stop_propagation();
                self.next(window, cx);
            }
            ("escape", Step::Look | Step::Connections | Step::Passwords) => {
                cx.stop_propagation();
                self.back(cx);
            }
            ("left", Step::Look) => {
                cx.stop_propagation();
                self.move_theme(-1, window, cx);
            }
            ("right", Step::Look) => {
                cx.stop_propagation();
                self.move_theme(1, window, cx);
            }
            _ => {}
        }
    }

    fn render_welcome(&self, cx: &mut Context<Self>) -> AnyElement {
        let t = tokens(cx);
        let feature = |icon: IconName, colour: Hsla, title: &str, text: &str| {
            v_flex()
                .w(dp(300.))
                .h(dp(168.))
                .gap(dp(10.))
                .p(dp(20.))
                .rounded(dp(16.))
                .border_1()
                .border_color(t.border)
                .bg(t.sidebar)
                .child(icon_tile(icon, colour, 40.))
                .child(
                    div()
                        .pt(dp(4.))
                        .text_size(dp(14.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(t.text)
                        .child(title.to_string()),
                )
                .child(
                    div()
                        .text_size(dp(12.5))
                        .text_color(t.text_secondary)
                        .child(text.to_string()),
                )
        };
        v_flex()
            .items_center()
            .gap(dp(26.))
            .child(
                div()
                    .rounded(dp(18.))
                    .shadow(vec![BoxShadow {
                        color: t.accent.opacity(0.35),
                        offset: point(px(0.), px(8.)),
                        blur_radius: px(40.),
                        spread_radius: px(0.),
            inset: false,
                    }])
                    .child(logo(dialog::width(80., cx), false)),
            )
            .child(
                v_flex()
                    .items_center()
                    .gap(dp(10.))
                    .child(
                        div()
                            .text_size(dp(38.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(t.text)
                            .child("Welcome to Tabula"),
                    )
                    .child(
                        div()
                            .text_size(dp(16.))
                            .text_color(t.text_secondary)
                            .child("A fast, native home for your databases: Postgres, MySQL, SQLite, Redis and MongoDB."),
                    ),
            )
            .child(
                h_flex()
                    .gap(dp(16.))
                    .items_start()
                    .child(feature(
                        IconName::Zap,
                        t.warning,
                        "Fast and native",
                        "Opens in a blink and scrolls millions of rows without stutter.",
                    ))
                    .child(feature(
                        IconName::ShieldCheck,
                        t.success,
                        "Safe by default",
                        "Edits wait for your Commit. Production always asks before it writes.",
                    ))
                    .child(feature(
                        IconName::Layers,
                        t.info,
                        "Every database",
                        "One window for SQL, Redis keys and MongoDB documents.",
                    )),
            )
            .child(
                v_flex()
                    .items_center()
                    .gap(dp(14.))
                    .pt(dp(8.))
                    .child(
                        div()
                            .debug_selector(|| "onboarding-start".into())
                            .w(dp(260.))
                            .rounded(dp(8.))
                            .shadow(accent_glow(&t))
                            .child(
                                Button::new("get-started")
                                    .cursor_pointer()
                                    .w_full()
                                    .h(dp(44.))
                                    .label("Get started")
                                    .primary()
                                    .on_click(cx.listener(|this, _, window, cx| this.next(window, cx))),
                            ),
                    )
                    .child(
                        Button::new("from-tableplus")
                            .cursor_pointer()
                            .label("Coming from TablePlus? Import your connections")
                            .ghost()
                            .small()
                            // Still goes through Look & feel, then lands on
                            // Connections with Import singled out.
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.import_first = true;
                                this.go(Step::Look, cx);
                            })),
                    ),
            )
            .child(
                div()
                    .pt(dp(20.))
                    .text_size(dp(12.))
                    .text_color(t.text_muted)
                    .child(if cfg!(target_os = "macos") {
                        "No account, no tracking. Your connections and passwords stay on this Mac."
                    } else {
                        "No account, no tracking. Your connections and passwords stay on this computer."
                    }),
            )
            .into_any_element()
    }

    fn render_look(&self, cx: &mut Context<Self>) -> AnyElement {
        let t = tokens(cx);
        let s = settings::get(cx);
        let theme_card = |index: usize, choice: Appearance, label: &str, note: &str| {
            let selected = s.appearance == choice;
            let preview = match choice {
                Appearance::System => h_flex()
                    .size_full()
                    .child(mini_window(tokens_for(true)).w(relative(0.5)))
                    .child(mini_window(tokens_for(false)).w(relative(0.5))),
                Appearance::Dark => h_flex().size_full().child(mini_window(tokens_for(true))),
                Appearance::Light => h_flex().size_full().child(mini_window(tokens_for(false))),
            };
            let settings_index = appearance_index(choice);
            v_flex()
                .id(("theme-card", index))
                .debug_selector(move || format!("theme-card-{index}"))
                .w(dp(280.))
                .h(dp(232.))
                .gap(dp(12.))
                .p(dp(14.))
                .rounded(dp(14.))
                .map(|card| {
                    if selected {
                        card.border_2().border_color(t.accent).bg(t.raised)
                    } else {
                        card.border_1().border_color(t.border).bg(t.sidebar)
                    }
                })
                .cursor_pointer()
                .hover(|style| style.bg(t.raised))
                .child(
                    div()
                        .h(dp(128.))
                        .rounded(dp(8.))
                        .overflow_hidden()
                        .border_1()
                        .border_color(t.border)
                        .child(preview),
                )
                .child(
                    h_flex().gap(dp(10.)).child(radio(selected, &t)).child(
                        v_flex()
                            .gap(dp(2.))
                            .child(
                                div()
                                    .text_size(dp(14.))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(t.text)
                                    .child(label.to_string()),
                            )
                            .child(
                                div()
                                    .text_size(dp(12.))
                                    .text_color(t.text_muted)
                                    .child(note.to_string()),
                            ),
                    ),
                )
                .on_click(move |_, window, cx| set_appearance(settings_index, window, cx))
        };
        let size_option = |index: usize, size: f32, label: &str| {
            let selected = s.ui_font_size == size;
            v_flex()
                .id(("text-size", index))
                .debug_selector(move || format!("text-size-{index}"))
                .flex_1()
                .items_center()
                .gap(dp(2.))
                .py(dp(8.))
                .rounded(dp(8.))
                .cursor_pointer()
                .when(selected, |option| option.bg(t.hover))
                .hover(|style| style.bg(t.hover))
                .child(
                    div()
                        .text_size(px(size * 1.25))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(if selected { t.text } else { t.text_secondary })
                        .child("Aa"),
                )
                .child(
                    div()
                        .text_size(dp(11.))
                        .text_color(t.text_muted)
                        .child(format!("{label} · {size} px")),
                )
                .on_click(move |_, _, cx| settings::update(cx, |s| s.ui_font_size = size))
        };
        let preview_cell = |title: &str, value: AnyElement| {
            v_flex()
                .flex_1()
                .gap(dp(6.))
                .child(
                    div()
                        .text_size(dp(12.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(t.text)
                        .child(title.to_string()),
                )
                .child(value)
        };
        let mono = |text: &str| {
            div()
                .font_family(gpui_mono(cx))
                .text_size(dp(12.))
                .text_color(t.text)
                .child(text.to_string())
                .into_any_element()
        };
        v_flex()
            .items_center()
            .gap(dp(24.))
            .child(step_title(
                "Make it yours",
                "Pick a look. You can change both any time in Settings (⌘,).",
                cx,
            ))
            .child(
                h_flex().gap(dp(16.)).children(
                    THEMES
                        .iter()
                        .enumerate()
                        .map(|(index, (choice, label, note))| {
                            theme_card(index, *choice, label, note)
                        }),
                ),
            )
            .child(
                v_flex()
                    .w(dp(872.))
                    .gap(dp(10.))
                    .child(
                        v_flex()
                            .gap(dp(2.))
                            .child(
                                div()
                                    .text_size(dp(13.))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(t.text)
                                    .child("Text size"),
                            )
                            .child(
                                div()
                                    .text_size(dp(12.))
                                    .text_color(t.text_muted)
                                    .child("Scales the whole interface, tables included."),
                            ),
                    )
                    .child(
                        h_flex()
                            .gap(dp(4.))
                            .p(dp(4.))
                            .rounded(dp(10.))
                            .border_1()
                            .border_color(t.border)
                            .bg(t.sidebar)
                            .children(
                                SIZES
                                    .iter()
                                    .enumerate()
                                    .map(|(index, (size, label))| size_option(index, *size, label)),
                            ),
                    )
                    .child(
                        v_flex()
                            .gap(dp(8.))
                            .px(dp(16.))
                            .py(dp(12.))
                            .rounded(dp(10.))
                            .border_1()
                            .border_color(t.border)
                            .bg(t.canvas)
                            .child(caps("Preview", cx))
                            .child(
                                h_flex()
                                    .items_start()
                                    .child(preview_cell("id", mono("10412")))
                                    .child(preview_cell("customer", mono("Priya Raman")))
                                    .child(preview_cell(
                                        "status",
                                        h_flex().child(enum_pill("paid", cx)).into_any_element(),
                                    ))
                                    .child(preview_cell("total", mono("249.00"))),
                            ),
                    ),
            )
            .child(nav_buttons(true, "Continue", cx))
            .into_any_element()
    }

    fn render_connections(&self, cx: &mut Context<Self>) -> AnyElement {
        let t = tokens(cx);
        let chip = |text: &str| {
            div()
                .px(dp(8.))
                .py(dp(3.))
                .rounded(dp(6.))
                .border_1()
                .border_color(t.border)
                .bg(t.canvas)
                .text_size(dp(11.))
                .text_color(t.text_secondary)
                .child(text.to_string())
                .into_any_element()
        };
        let engine = |label: &str, colour: Hsla| {
            div()
                .px(dp(6.))
                .py(dp(2.))
                .rounded(dp(5.))
                .bg(colour.opacity(0.16))
                .text_size(dp(10.5))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(colour)
                .child(label.to_string())
                .into_any_element()
        };
        let card = |id: &'static str,
                    icon: IconName,
                    colour: Hsla,
                    title: &str,
                    badge: Option<&str>,
                    text: &str,
                    chips: Vec<AnyElement>,
                    picked: Picked,
                    stand_out: bool,
                    cx: &mut Context<Self>| {
            v_flex()
                .id(id)
                .debug_selector(move || id.to_string())
                .w(dp(362.))
                .h(dp(170.))
                .gap(dp(10.))
                .p(dp(18.))
                .rounded(dp(14.))
                .border_1()
                .border_color(if stand_out { t.accent } else { t.border })
                .bg(if stand_out { t.raised } else { t.sidebar })
                .cursor_pointer()
                .hover(|style| style.bg(t.raised))
                .child(
                    h_flex()
                        .items_start()
                        .gap(dp(12.))
                        .child(icon_tile(icon, colour, 36.))
                        .child(
                            v_flex()
                                .flex_1()
                                .min_w_0()
                                .gap(dp(4.))
                                .child(
                                    h_flex()
                                        .gap(dp(8.))
                                        .child(
                                            div()
                                                .text_size(dp(14.))
                                                .font_weight(FontWeight::SEMIBOLD)
                                                .text_color(t.text)
                                                .child(title.to_string()),
                                        )
                                        .children(badge.map(|badge| {
                                            div()
                                                .px(dp(6.))
                                                .py(dp(1.))
                                                .rounded(dp(5.))
                                                .bg(t.warning.opacity(0.18))
                                                .text_size(dp(10.5))
                                                .font_weight(FontWeight::SEMIBOLD)
                                                .text_color(t.warning)
                                                .child(badge.to_string())
                                        })),
                                )
                                .child(
                                    div()
                                        .text_size(dp(12.))
                                        .text_color(t.text_secondary)
                                        .child(text.to_string()),
                                ),
                        )
                        .child(
                            Icon::new(IconName::ChevronRight)
                                .small()
                                .text_color(t.text_faint),
                        ),
                )
                .child(div().flex_1())
                .child(h_flex().pl(dp(48.)).gap(dp(6.)).children(chips))
                .on_click(cx.listener(move |this, _, window, cx| this.pick(picked, window, cx)))
        };
        // Import's chips are choices of their own.
        let import_chip =
            |id: &'static str, label: &str, picked: Picked, cx: &mut Context<Self>| {
                div()
                    .id(id)
                    .debug_selector(move || id.to_string())
                    .px(dp(10.))
                    .py(dp(3.))
                    .rounded(dp(6.))
                    .border_1()
                    .border_color(t.border)
                    .bg(t.canvas)
                    .text_size(dp(11.))
                    .text_color(t.text)
                    .cursor_pointer()
                    .hover(|style| style.bg(t.hover))
                    .child(label.to_string())
                    .on_click(cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        this.pick(picked, window, cx)
                    }))
                    .into_any_element()
            };
        let import_chips = vec![
            import_chip("import-from-file", "From a file…", Picked::ImportFile, cx),
            import_chip("import-from-mac", "From this Mac", Picked::ImportMac, cx),
        ];
        v_flex()
            .items_center()
            .gap(dp(24.))
            .child(step_title(
                "Add your first database",
                "Bring in what you already have, or start fresh. You can add more any time.",
                cx,
            ))
            .child(
                v_flex()
                    .gap(dp(16.))
                    .child(
                        h_flex()
                            .gap(dp(16.))
                            .child(card(
                                "pick-import",
                                IconName::Download,
                                t.accent,
                                "Import from TablePlus",
                                None,
                                "Bring over your saved connections, including passwords, from an export or TablePlus on this Mac.",
                                import_chips,
                                Picked::Import,
                                self.import_first,
                                cx,
                            ))
                            .child(card(
                                "pick-new",
                                IconName::Plus,
                                t.success,
                                "New connection",
                                None,
                                "Postgres, MySQL, SQLite, Redis or MongoDB, with SSH tunnels and TLS. Paste a connection URL to fill it in.",
                                vec![
                                    engine("PG", t.info),
                                    engine("MY", t.warning),
                                    engine("RD", t.danger),
                                    engine("MG", t.success),
                                ],
                                Picked::NewConnection,
                                false,
                                cx,
                            )),
                    )
                    .child(
                        h_flex()
                            .gap(dp(16.))
                            .child(card(
                                "pick-sqlite",
                                IconName::FolderOpen,
                                t.number,
                                "Open a SQLite file",
                                None,
                                "Pick a .sqlite or .db file. No server and no password needed.",
                                vec![chip(".sqlite"), chip(".db"), chip(".sqlite3")],
                                Picked::SqliteFile,
                                false,
                                cx,
                            ))
                            .child(card(
                                "pick-sample",
                                IconName::Sparkles,
                                t.warning,
                                "Try the sample database",
                                Some("No setup"),
                                "A small shop database (customers, orders, products) to explore safely. Delete it whenever you like.",
                                vec![chip("3 tables"), chip("~2,000 rows"), chip("SQLite")],
                                Picked::Sample,
                                false,
                                cx,
                            )),
                    ),
            )
            .child(
                v_flex()
                    .items_center()
                    .gap(dp(8.))
                    .child(
                        div().debug_selector(|| "onboarding-later".into()).child(
                            Button::new("later")
                                .cursor_pointer()
                                .label("I'll do this later")
                                .ghost()
                                .small()
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.pick(Picked::Nothing, window, cx)
                                })),
                        ),
                    )
                    .child(
                        div()
                            .text_size(dp(11.5))
                            .text_color(t.text_muted)
                            .child("Passwords are only asked for once you pick an option that needs them."),
                    ),
            )
            .child(nav_buttons(false, "", cx))
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
        let size_name = SIZES
            .iter()
            .find(|(size, _)| *size == s.ui_font_size)
            .map(|(_, name)| *name)
            .unwrap_or("Custom");
        let saved = self.home.read(cx).connections.len();
        let connections = match (self.picked, saved) {
            (Picked::Sample, _) => Some("Sample database added".to_string()),
            (Picked::Import | Picked::ImportFile | Picked::ImportMac, count) if count > 0 => {
                Some(format!("Imported from TablePlus ({count} saved)"))
            }
            (_, 0) => None,
            (_, 1) => Some("1 connection saved".to_string()),
            (_, count) => Some(format!("{count} connections saved")),
        };
        let passwords = if !s.save_passwords {
            Some("Not saved: asked at connect".to_string())
        } else if s.keychain_explained {
            Some(format!("Saved in {}", keychain_name()))
        } else {
            None
        };
        let summary_row = |icon: IconName, label: &str, value: Option<String>, last: bool| {
            let done = value.is_some();
            h_flex()
                .gap(dp(14.))
                .px(dp(18.))
                .h(dp(46.))
                .when(!last, |row| row.border_b_1().border_color(t.border))
                .child(Icon::new(icon).small().text_color(t.text_secondary))
                .child(
                    div()
                        .w(dp(120.))
                        .text_size(dp(13.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(t.text)
                        .child(label.to_string()),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_size(dp(12.5))
                        .text_color(if done { t.text_secondary } else { t.text_muted })
                        .child(
                            value
                                .unwrap_or_else(|| "Not set up: add later from Connections".into()),
                        ),
                )
                .child(if done {
                    Icon::new(IconName::Check)
                        .small()
                        .text_color(t.success)
                        .into_any_element()
                } else {
                    div().text_color(t.text_faint).child("—").into_any_element()
                })
        };
        let shortcut = |keys: &str, what: &str| {
            v_flex()
                .w(dp(137.))
                .h(dp(96.))
                .gap(dp(10.))
                .p(dp(14.))
                .rounded(dp(12.))
                .border_1()
                .border_color(t.border)
                .bg(t.sidebar)
                .child(h_flex().child(kbd(keys.to_string(), cx)))
                .child(
                    div()
                        .text_size(dp(12.5))
                        .text_color(t.text_secondary)
                        .child(what.to_string()),
                )
        };
        v_flex()
            .items_center()
            .gap(dp(24.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_center()
                    .size(dp(56.))
                    .rounded_full()
                    .bg(t.success.opacity(0.16))
                    .child(Icon::new(IconName::Check).text_color(t.success)),
            )
            .child(step_title(
                "You're all set",
                "Here's what Tabula set up, and four shortcuts worth knowing.",
                cx,
            ))
            .child(
                v_flex()
                    .w(dp(580.))
                    .rounded(dp(14.))
                    .border_1()
                    .border_color(t.border)
                    .bg(t.sidebar)
                    .child(summary_row(
                        IconName::Sun,
                        "Appearance",
                        Some(format!(
                            "{theme} theme · {size_name} text ({} px)",
                            s.ui_font_size
                        )),
                        false,
                    ))
                    .child(summary_row(
                        IconName::Database,
                        "Connections",
                        connections,
                        false,
                    ))
                    .child(summary_row(
                        IconName::KeyRound,
                        "Passwords",
                        passwords,
                        true,
                    )),
            )
            .child(
                v_flex()
                    .w(dp(580.))
                    .gap(dp(10.))
                    .child(caps("Shortcuts", cx))
                    .child(
                        h_flex()
                            .gap(dp(10.))
                            .child(shortcut("⌘K", "Switch database"))
                            .child(shortcut("⌘P", "Go to any table or command"))
                            .child(shortcut("⌘↵", "Run the query"))
                            .child(shortcut("⌘S", "Commit your edits")),
                    ),
            )
            .child(
                v_flex()
                    .items_center()
                    .gap(dp(10.))
                    .child(
                        div()
                            .debug_selector(|| "onboarding-finish".into())
                            .w(dp(194.))
                            .rounded(dp(8.))
                            .shadow(accent_glow(&t))
                            .child(
                                Button::new("open-tabula")
                                    .cursor_pointer()
                                    .w_full()
                                    .h(dp(40.))
                                    .label("Open Tabula")
                                    .primary()
                                    .on_click(
                                        cx.listener(|this, _, window, cx| this.finish(window, cx)),
                                    ),
                            ),
                    )
                    .child(
                        div()
                            .text_size(dp(11.5))
                            .text_color(t.text_muted)
                            .child("Change any of this later in Settings (⌘,)."),
                    ),
            )
            .into_any_element()
    }
}

/// Settings' appearance index (System, Light, Dark) for a theme.
fn appearance_index(appearance: Appearance) -> usize {
    match appearance {
        Appearance::System => 0,
        Appearance::Light => 1,
        Appearance::Dark => 2,
    }
}

/// The accent glow under a primary button.
fn accent_glow(t: &Tokens) -> Vec<BoxShadow> {
    vec![BoxShadow {
        color: t.accent.opacity(0.4),
        offset: point(px(0.), px(6.)),
        blur_radius: px(24.),
        spread_radius: px(0.),
        inset: false,
    }]
}

/// A tinted square with an icon (feature cards, connection cards).
fn icon_tile(icon: IconName, colour: Hsla, size: f32) -> Div {
    div()
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(dp(size))
        .rounded(dp(size / 4.))
        .bg(colour.opacity(0.16))
        .child(Icon::new(icon).text_color(colour))
}

/// A radio dot, filled when selected.
fn radio(selected: bool, t: &Tokens) -> Div {
    div()
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(dp(16.))
        .rounded_full()
        .border_2()
        .border_color(if selected { t.accent } else { t.text_faint })
        .when(selected, |dot| {
            dot.child(div().size(dp(6.)).rounded_full().bg(t.accent))
        })
}

/// The monospace family, for the preview row.
fn gpui_mono(cx: &App) -> SharedString {
    cx.theme().mono_font_family.clone()
}

/// A step's centred heading and a muted line.
fn step_title(title: &str, sub: &str, cx: &App) -> AnyElement {
    let t = tokens(cx);
    v_flex()
        .items_center()
        .gap(dp(8.))
        .child(
            div()
                .text_size(dp(30.))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(t.text)
                .child(title.to_string()),
        )
        .child(
            div()
                .text_size(dp(14.))
                .text_color(t.text_secondary)
                .child(sub.to_string()),
        )
        .into_any_element()
}

/// Back (and Continue when `next` isn't empty), centred under a step.
fn nav_buttons(with_next: bool, next: &str, cx: &mut Context<Onboarding>) -> AnyElement {
    h_flex()
        .gap(dp(10.))
        .justify_center()
        .child(
            div().debug_selector(|| "onboarding-back".into()).child(
                Button::new("onboarding-back")
                    .cursor_pointer()
                    .w(dp(112.))
                    .label("Back")
                    .outline()
                    .on_click(cx.listener(|this, _, _, cx| this.back(cx))),
            ),
        )
        .when(with_next, |row| {
            row.child(
                div().debug_selector(|| "onboarding-next".into()).child(
                    Button::new("onboarding-next")
                        .cursor_pointer()
                        .w(dp(112.))
                        .label(next.to_string())
                        .primary()
                        .on_click(cx.listener(|this, _, window, cx| this.next(window, cx))),
                ),
            )
        })
        .into_any_element()
}

/// A tiny window drawn in one mode's colours, for the theme cards.
fn mini_window(t: Tokens) -> Div {
    let bar =
        |color: Hsla, width: f32| div().h(dp(3.)).w(relative(width)).rounded(dp(2.)).bg(color);
    h_flex()
        .size_full()
        .bg(t.canvas)
        .child(
            v_flex()
                .w(dp(44.))
                .h_full()
                .p(dp(7.))
                .gap(dp(6.))
                .bg(t.sidebar)
                .child(bar(t.text_muted, 0.8))
                .child(bar(t.text_faint, 0.6))
                .child(bar(t.accent, 0.8))
                .child(bar(t.text_faint, 0.7))
                .child(bar(t.text_faint, 0.5)),
        )
        .child(
            v_flex()
                .flex_1()
                .p(dp(8.))
                .gap(dp(7.))
                .child(bar(t.text, 0.3))
                .child(bar(t.text_muted, 0.9))
                .child(bar(t.text_faint, 0.8))
                .child(bar(t.text_faint, 0.85))
                .child(bar(t.text_faint, 0.7)),
        )
}

/// A blurred-looking glow: many faint rings, each a little smaller, so
/// the steps are too small to see (gpui has no blur or radial gradient).
fn glow(colour: Hsla, diameter: Pixels, strength: f32) -> Div {
    const RINGS: usize = 24;
    div()
        .absolute()
        .size(diameter)
        .children((0..RINGS).map(|ring| {
            let share = 1. - ring as f32 / RINGS as f32;
            let size = diameter * share;
            div()
                .absolute()
                .left((diameter - size) / 2.)
                .top((diameter - size) / 2.)
                .size(size)
                .rounded_full()
                .bg(colour.opacity(0.0065 * strength))
        }))
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
            Step::Passwords => match &self.passwords {
                Some(step) => div().w(dp(900.)).child(step.clone()).into_any_element(),
                None => div().into_any_element(),
            },
            Step::Done => self.render_done(cx),
        };
        let segments = self.segments(cx);
        let at = segments.iter().position(|(step, _)| *step == self.step);
        let dark = cx.theme().mode.is_dark();
        div()
            .id("onboarding")
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key))
            .relative()
            .size_full()
            .overflow_hidden()
            .bg(t.canvas)
            // A soft accent glow at the top and a teal one lower right.
            .child(
                glow(t.accent, px(1100.), if dark { 1. } else { 0.5 })
                    .left(relative(0.5))
                    .ml(px(-550.))
                    .top(px(-520.)),
            )
            .child(
                glow(t.number, px(800.), if dark { 0.55 } else { 0.3 })
                    .right(px(-260.))
                    .bottom(px(-320.)),
            )
            .child(
                div()
                    .id("onboarding-scroll")
                    .relative()
                    .size_full()
                    .overflow_y_scroll()
                    .child(
                        v_flex()
                            .w_full()
                            .min_h_full()
                            .items_center()
                            .justify_center()
                            .px(dp(32.))
                            .pt(dp(32.))
                            .pb(dp(110.))
                            .child(
                                div()
                                    .w_full()
                                    .max_w(dp(940.))
                                    .flex()
                                    .justify_center()
                                    .child(body),
                            ),
                    ),
            )
            // Progress, under the step.
            .when_some(at, |screen, at| {
                screen.child(
                    v_flex()
                        .absolute()
                        .bottom(dp(36.))
                        .left_0()
                        .right_0()
                        .items_center()
                        .gap(dp(8.))
                        .child(
                            h_flex()
                                .gap(dp(8.))
                                .children(segments.iter().enumerate().map(|(index, _)| {
                                    div()
                                        .h(dp(4.))
                                        .w(dp(28.))
                                        .rounded_full()
                                        .bg(if index <= at { t.accent } else { t.hover })
                                })),
                        )
                        .child(
                            div()
                                .text_size(dp(11.))
                                .text_color(t.text_muted)
                                .child(format!(
                                    "Step {} of {} · {}",
                                    at + 1,
                                    segments.len(),
                                    segments[at].1
                                )),
                        ),
                )
            })
            .when(self.step != Step::Done, |screen| {
                screen.child(
                    div()
                        .absolute()
                        .right(dp(24.))
                        .bottom(dp(20.))
                        .debug_selector(|| "onboarding-skip".into())
                        .child(
                            Button::new("skip-setup")
                                .cursor_pointer()
                                .label("Skip setup")
                                .ghost()
                                .small()
                                .on_click(
                                    cx.listener(|this, _, window, cx| this.finish(window, cx)),
                                ),
                        ),
                )
            })
    }
}
