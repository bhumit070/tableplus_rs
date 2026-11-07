//! Settings dialog (design.md, screen 21): General, Appearance, Editor,
//! Data grid, Safe mode, and a searchable list of every shortcut, generated
//! from the key bindings. Changes apply at once; there is no Save.

use std::collections::HashMap;
use std::rc::Rc;

use gpui::{
    AnyElement, App, AppContext as _, Context, Entity, FontWeight, IntoElement, ParentElement,
    Render, SharedString, Styled, Subscription, Window, div, prelude::*,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::checkbox::Checkbox;
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::kbd::Kbd;
use gpui_component::select::{Select, SelectEvent, SelectState};
use gpui_component::{
    ActiveTheme as _, Icon, IndexPath, Sizable as _, Theme, ThemeMode, WindowExt as _, h_flex,
    v_flex,
};
use gpui_kit_assets::IconName;

use super::dialog;
use super::key_bindings;
use super::theme::{caps, dp, kbd, keychain_name, tokens};
use crate::db::SafeMode;
use crate::menus;
use crate::settings::{self, Appearance, EDITOR_RANGE, PAGE_SIZES, Settings, UI_RANGE};
use crate::ui::DisabledWhen as _;
use gpui::{AsKeystroke, MenuItem, px};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Section {
    General,
    Appearance,
    Editor,
    DataGrid,
    SafeMode,
    Shortcuts,
}

const SECTIONS: [(Section, &str, IconName); 6] = [
    (Section::General, "General", IconName::Settings),
    (Section::Appearance, "Appearance", IconName::Palette),
    (Section::Editor, "Editor", IconName::SquareCode),
    (Section::DataGrid, "Data grid", IconName::Table),
    (Section::SafeMode, "Safe mode", IconName::Shield),
    (Section::Shortcuts, "Shortcuts", IconName::Keyboard),
];

/// One row of the shortcut list.
struct Shortcut {
    name: String,
    keys: String,
    /// Where it applies, when not everywhere ("Data grid", "Cell editor").
    scope: Option<&'static str>,
}

pub struct SettingsView {
    section: Section,
    search: Entity<InputState>,
    page_size: Entity<SelectState<Vec<String>>>,
    safe_mode: Entity<SelectState<Vec<&'static str>>>,
    shortcuts: Vec<Shortcut>,
    _subs: Vec<Subscription>,
}

impl SettingsView {
    pub fn open(window: &mut Window, cx: &mut App) -> Entity<Self> {
        let view = cx.new(|cx| SettingsView::new(window, cx));
        let w = dialog::width(760., cx);
        let shown = view.clone();
        window.open_dialog(cx, move |d, _, cx| {
            dialog::shell(d, w, cx).child(shown.clone())
        });
        view
    }

    /// For screenshots: show the shortcut list.
    #[cfg(test)]
    pub fn show_shortcuts(&mut self, cx: &mut Context<Self>) {
        self.section = Section::Shortcuts;
        cx.notify();
    }

    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let s = settings::get(cx);
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search shortcuts…"));
        let pi = PAGE_SIZES
            .iter()
            .position(|n| *n == s.page_size)
            .unwrap_or(1);
        let page_size = cx.new(|cx| {
            SelectState::new(
                PAGE_SIZES.iter().map(|n| format!("{n} rows")).collect(),
                Some(IndexPath::new(pi)),
                window,
                cx,
            )
        });
        let si = SafeMode::ALL
            .iter()
            .position(|m| *m == s.default_safe_mode)
            .unwrap_or(0);
        let safe_mode = cx.new(|cx| {
            SelectState::new(
                SafeMode::ALL.iter().map(|m| m.label()).collect(),
                Some(IndexPath::new(si)),
                window,
                cx,
            )
        });
        let subs = vec![
            // Follow changes made here, via ⌘= / ⌘−, or by Reset.
            cx.observe_global_in::<Settings>(window, |this, window, cx| {
                let s = settings::get(cx);
                let pi = PAGE_SIZES.iter().position(|n| *n == s.page_size);
                let si = SafeMode::ALL.iter().position(|m| *m == s.default_safe_mode);
                // Only when they disagree, so this can't feed back into itself.
                if this.page_size.read(cx).selected_index(cx) != pi.map(IndexPath::new) {
                    this.page_size.update(cx, |st, cx| {
                        st.set_selected_index(pi.map(IndexPath::new), window, cx)
                    });
                }
                if this.safe_mode.read(cx).selected_index(cx) != si.map(IndexPath::new) {
                    this.safe_mode.update(cx, |st, cx| {
                        st.set_selected_index(si.map(IndexPath::new), window, cx)
                    });
                }
                cx.notify();
            }),
            cx.subscribe_in(&search, window, |_, _, ev: &InputEvent, _, cx| {
                if let InputEvent::Change = ev {
                    cx.notify();
                }
            }),
            cx.subscribe_in(
                &page_size,
                window,
                |_, _, ev: &SelectEvent<Vec<String>>, _, cx| {
                    if let SelectEvent::Confirm(Some(label)) = ev
                        && let Some(n) =
                            label.split_whitespace().next().and_then(|n| n.parse().ok())
                    {
                        settings::update(cx, |s| s.page_size = n);
                    }
                },
            ),
            cx.subscribe_in(
                &safe_mode,
                window,
                |_, _, ev: &SelectEvent<Vec<&'static str>>, _, cx| {
                    if let SelectEvent::Confirm(Some(label)) = ev
                        && let Some(m) = SafeMode::ALL.into_iter().find(|m| m.label() == *label)
                    {
                        settings::update(cx, |s| s.default_safe_mode = m);
                    }
                },
            ),
        ];
        Self {
            section: Section::General,
            search,
            page_size,
            safe_mode,
            shortcuts: shortcuts(),
            _subs: subs,
        }
    }

    fn render_section(
        &self,
        s: Settings,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let t = tokens(cx);
        match self.section {
            Section::General => v_flex()
                .gap(dp(16.))
                .child(row(
                    "Offer last session at launch",
                    "Connections and tabs, including unsaved query text, can come back when Tabula starts.",
                    Checkbox::new("restore-sessions")
                        .cursor_pointer()
                        .checked(s.restore_sessions)
                        .on_click(|v: &bool, _, cx| {
                            let v = *v;
                            settings::update(cx, |s| s.restore_sessions = v)
                        }),
                    cx,
                ))
                .when(s.restore_sessions, |d| {
                    d.child(row(
                        "Reopen without asking",
                        "Connects right away at launch (macOS may then ask for the keychain before the window is up).",
                        Checkbox::new("always-reopen-setting")
                            .cursor_pointer()
                            .checked(s.always_reopen)
                            .on_click(|v: &bool, _, cx| {
                                let v = *v;
                                settings::update(cx, |s| s.always_reopen = v)
                            }),
                        cx,
                    ))
                })
                .child(row(
                    "Save passwords",
                    if s.save_passwords {
                        format!("In {}, never in Tabula's files.", keychain_name())
                    } else {
                        "Off: nothing is saved; Tabula asks when you connect.".to_string()
                    },
                    Checkbox::new("save-passwords")
                        .cursor_pointer()
                        .checked(s.save_passwords)
                        .on_click(|on: &bool, _, cx| {
                            let on = *on;
                            settings::update(cx, |s| s.save_passwords = on)
                        }),
                    cx,
                ))
                .into_any_element(),
            Section::Appearance => {
                let ai = match s.appearance {
                    Appearance::System => 0,
                    Appearance::Light => 1,
                    Appearance::Dark => 2,
                };
                v_flex()
                    .gap(dp(16.))
                    .child(row(
                        "Theme",
                        "⌘⇧L switches between light and dark.",
                        dialog::segmented(
                            "appearance",
                            &["System", "Light", "Dark"],
                            ai,
                            Rc::new(|i, window, cx| set_appearance(i, window, cx)),
                            cx,
                        )
                        .w(dp(240.)),
                        cx,
                    ))
                    .child(row(
                        "Interface size",
                        "Scales the whole window: tables, sidebar, dialogs. ⌘= / ⌘− zoom.",
                        stepper("ui-size", s.ui_font_size, UI_RANGE, |s, v| s.ui_font_size = v),
                        cx,
                    ))
                    .into_any_element()
            }
            Section::Editor => v_flex()
                .gap(dp(16.))
                .child(row(
                    "Font size",
                    "SQL and query editor text.",
                    stepper("editor-size", s.editor_font_size, EDITOR_RANGE, |s, v| {
                        s.editor_font_size = v
                    }),
                    cx,
                ))
                .child(
                    div()
                        .p(dp(12.))
                        .rounded(dp(8.))
                        .border_1()
                        .border_color(t.border)
                        .bg(t.canvas)
                        .font_family(cx.theme().mono_font_family.clone())
                        .text_size(px(s.editor_font_size))
                        .child(
                            h_flex()
                                .child(div().text_color(t.keyword).child("SELECT"))
                                .child(" * ")
                                .child(div().text_color(t.keyword).child("FROM"))
                                .child(" users ")
                                .child(div().text_color(t.keyword).child("WHERE"))
                                .child(" id = ")
                                .child(div().text_color(t.warning).child("1"))
                                .child(";"),
                        ),
                )
                .into_any_element(),
            Section::DataGrid => v_flex()
                .gap(dp(16.))
                .child(row(
                    "Rows per page",
                    "How many rows a table shows when it opens; each tab can change it.",
                    Select::new(&self.page_size).cursor_pointer().w(dp(160.)),
                    cx,
                ))
                .into_any_element(),
            Section::SafeMode => v_flex()
                .gap(dp(16.))
                .child(row(
                    "New connections start with",
                    "Automatic confirms writes on Production and nowhere else. Each connection can change it.",
                    Select::new(&self.safe_mode).cursor_pointer().w(dp(220.)),
                    cx,
                ))
                .child(
                    v_flex()
                        .gap(dp(6.))
                        .text_size(dp(12.))
                        .text_color(t.text_muted)
                        .child("Confirm writes: shows the SQL and asks before anything that writes, and before commits.")
                        .child("Confirm every query: asks before every query.")
                        .child("Read-only: refuses writes; the server session is read-only where possible."),
                )
                .into_any_element(),
            Section::Shortcuts => self.render_shortcuts(window, cx),
        }
    }

    fn render_shortcuts(&self, _: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let t = tokens(cx);
        let q = self.search.read(cx).value().trim().to_lowercase();
        let rows: Vec<AnyElement> = self
            .shortcuts
            .iter()
            .filter(|s| {
                q.is_empty()
                    || s.name.to_lowercase().contains(&q)
                    || s.keys.to_lowercase().contains(&q)
                    || s.scope.is_some_and(|sc| sc.to_lowercase().contains(&q))
            })
            .map(|s| {
                h_flex()
                    .h(dp(30.))
                    .px(dp(8.))
                    .gap(dp(8.))
                    .rounded(dp(6.))
                    .hover(|st| st.bg(t.hover))
                    .child(
                        div()
                            .text_size(dp(12.5))
                            .text_color(t.text)
                            .child(s.name.clone()),
                    )
                    .when_some(s.scope, |d, scope| {
                        d.child(
                            div()
                                .text_size(dp(11.))
                                .text_color(t.text_faint)
                                .child(scope),
                        )
                    })
                    .child(div().flex_1())
                    .child(kbd(s.keys.clone(), cx))
                    .into_any_element()
            })
            .collect();
        let empty = rows.is_empty();
        v_flex()
            .gap(dp(10.))
            .child(
                Input::new(&self.search)
                    .prefix(Icon::new(IconName::Search).small().text_color(t.text_muted)),
            )
            .child(
                div()
                    .id("shortcut-list")
                    .h(dp(330.))
                    .overflow_y_scroll()
                    .child(v_flex().children(rows))
                    .when(empty, |d| {
                        d.child(
                            div()
                                .p(dp(12.))
                                .text_size(dp(12.5))
                                .text_color(t.text_muted)
                                .child("No shortcut matches."),
                        )
                    }),
            )
            .into_any_element()
    }
}

/// Switches light / dark / system and remembers it.
pub fn set_appearance(i: usize, window: &mut Window, cx: &mut App) {
    let appearance = [Appearance::System, Appearance::Light, Appearance::Dark][i];
    match appearance {
        Appearance::System => Theme::sync_system_appearance(Some(window), cx),
        Appearance::Light => Theme::change(ThemeMode::Light, Some(window), cx),
        Appearance::Dark => Theme::change(ThemeMode::Dark, Some(window), cx),
    }
    // Also puts the user's sizes back, which a theme switch can reload.
    settings::update(cx, |s| s.appearance = appearance);
}

/// A setting: name and a muted line on the left, the control on the right.
fn row(
    name: &'static str,
    note: impl Into<SharedString>,
    control: impl IntoElement,
    cx: &App,
) -> impl IntoElement {
    let t = tokens(cx);
    h_flex()
        .gap(dp(16.))
        .child(
            v_flex()
                .flex_1()
                .gap(dp(3.))
                .child(
                    div()
                        .text_size(dp(13.))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(t.text)
                        .child(name),
                )
                .child(
                    div()
                        .text_size(dp(12.))
                        .text_color(t.text_muted)
                        .child(note.into()),
                ),
        )
        .child(control)
}

/// −, value, + for one size setting.
fn stepper(
    id: &'static str,
    value: f32,
    range: (f32, f32),
    set: fn(&mut Settings, f32),
) -> impl IntoElement {
    h_flex()
        .gap(dp(6.))
        .child(
            Button::new((id, 0usize))
                .cursor_pointer()
                .icon(IconName::Minus)
                .outline()
                .small()
                .disabled_when(value <= range.0)
                .on_click(move |_, _, cx| settings::update(cx, |s| set(s, value - 1.))),
        )
        .child(
            div()
                .w(dp(52.))
                .text_center()
                .text_size(dp(12.5))
                .child(format!("{value} px")),
        )
        .child(
            Button::new((id, 1usize))
                .cursor_pointer()
                .icon(IconName::Plus)
                .outline()
                .small()
                .disabled_when(value >= range.1)
                .on_click(move |_, _, cx| settings::update(cx, |s| set(s, value + 1.))),
        )
}

/// Every shortcut, from the key bindings. Names come from the menu bar
/// where an action is there, otherwise from the action's name.
fn shortcuts() -> Vec<Shortcut> {
    let mut names = HashMap::new();
    fn walk(items: &[MenuItem], names: &mut HashMap<String, String>) {
        for item in items {
            match item {
                MenuItem::Submenu(m) => walk(&m.items, names),
                MenuItem::Action { name, action, .. } => {
                    names
                        .entry(action.name().to_string())
                        .or_insert_with(|| name.trim_end_matches('…').to_string());
                }
                _ => {}
            }
        }
    }
    for menu in menus() {
        walk(&menu.items, &mut names);
    }
    let mut out: Vec<Shortcut> = vec![];
    for b in key_bindings() {
        let action = b.action().name().to_string();
        let keys = b
            .keystrokes()
            .iter()
            .map(|k| Kbd::format(AsKeystroke::as_keystroke(k)))
            .collect::<Vec<_>>()
            .join(" ");
        let scope = b.predicate().map(|p| scope_name(&p.to_string()));
        let name = names
            .get(&action)
            .cloned()
            .unwrap_or_else(|| humanize(&action));
        // The same key and action in two contexts shows once, as global if
        // either is.
        if let Some(seen) = out.iter_mut().find(|s| s.name == name && s.keys == keys) {
            if scope.is_none() {
                seen.scope = None;
            }
            continue;
        }
        out.push(Shortcut { name, keys, scope });
    }
    out
}

/// "tabula::ui::CellUpMore" → "Cell up more".
fn humanize(action: &str) -> String {
    let base = action.rsplit("::").next().unwrap_or(action);
    let mut out = String::new();
    for (i, ch) in base.chars().enumerate() {
        if ch.is_uppercase() && i > 0 {
            out.push(' ');
            out.extend(ch.to_lowercase());
        } else {
            out.push(ch);
        }
    }
    out
}

/// Where a context-bound shortcut works.
fn scope_name(predicate: &str) -> &'static str {
    if predicate.contains("CellChoices") {
        "Cell dropdown"
    } else if predicate.contains("CellEditor") {
        "Cell editor"
    } else if predicate.contains("EditableGrid") || predicate.contains("DataTable") {
        "Data grid"
    } else if predicate.contains("QueryTab") {
        "Query editor"
    } else if predicate.contains("Input") {
        "Outside text fields"
    } else {
        "In context"
    }
}

impl Render for SettingsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = tokens(cx);
        let s = settings::get(cx);
        let nav = v_flex()
            .w(dp(170.))
            .flex_none()
            .gap(dp(2.))
            .children(SECTIONS.iter().map(|&(sec, label, icon)| {
                let active = sec == self.section;
                h_flex()
                    .id(SharedString::from(format!("settings-{label}")))
                    .debug_selector(move || format!("settings-{label}"))
                    .h(dp(30.))
                    .px(dp(10.))
                    .gap(dp(8.))
                    .rounded(dp(7.))
                    .cursor_pointer()
                    .text_size(dp(12.5))
                    .map(|d| {
                        if active {
                            d.bg(t.hover)
                                .text_color(t.text)
                                .font_weight(FontWeight::MEDIUM)
                        } else {
                            d.text_color(t.text_secondary)
                                .hover(|st| st.bg(t.hover.opacity(0.6)))
                        }
                    })
                    .child(Icon::new(icon).small().text_color(if active {
                        t.accent
                    } else {
                        t.text_muted
                    }))
                    .child(label)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.section = sec;
                        cx.notify();
                    }))
            }));
        let title = SECTIONS
            .iter()
            .find(|(sec, _, _)| *sec == self.section)
            .map(|(_, l, _)| *l)
            .unwrap_or_default();
        let content = self.render_section(s, window, cx);
        v_flex()
            .gap(dp(16.))
            .child(dialog::title("Settings", None, cx))
            .child(
                h_flex()
                    .items_start()
                    .gap(dp(20.))
                    .h(dp(420.))
                    .child(nav)
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .pl(dp(20.))
                            .border_l_1()
                            .border_color(t.border)
                            .gap(dp(16.))
                            .child(caps(title, cx))
                            .child(content),
                    ),
            )
            .child(dialog::footer(
                "Changes apply right away.",
                vec![
                    Button::new("reset-settings")
                        .cursor_pointer()
                        .label("Reset to defaults")
                        .ghost()
                        .disabled_when(s == Settings::default())
                        .on_click(|_, window, cx| {
                            settings::update(cx, |s| *s = Settings::default());
                            set_appearance(0, window, cx);
                        })
                        .into_any_element(),
                    Button::new("close-settings")
                        .cursor_pointer()
                        .label("Done")
                        .primary()
                        .on_click(|_, w, cx| w.close_dialog(cx))
                        .into_any_element(),
                ],
                cx,
            ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn humanizes_action_names() {
        assert_eq!(humanize("tabula::ui::CellUpMore"), "Cell up more");
        assert_eq!(humanize("SetNull"), "Set null");
    }

    #[test]
    fn lists_every_binding_with_menu_names() {
        let list = shortcuts();
        let run = list
            .iter()
            .find(|s| s.name == "Run Statement")
            .expect("run listed");
        assert_eq!(run.scope, None, "listed once, as global");
        assert_eq!(list.iter().filter(|s| s.name == "Run Statement").count(), 1);
        assert!(list.iter().any(|s| s.name == "Export"));
        assert!(
            run.keys.contains('⌘') || run.keys.contains("Ctrl"),
            "{}",
            run.keys
        );
        assert!(list.iter().any(|s| s.scope == Some("Cell editor")));
        assert!(list.len() > 30);
    }
}
