//! ⌘K database switcher: type to filter, ↑/↓ to move, Enter to switch.

use std::rc::Rc;

use gpui::{
    App, Context, Entity, IntoElement, KeyDownEvent, ParentElement, Render, Styled, Subscription,
    Window, div, prelude::*, px,
};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::{ActiveTheme as _, Icon, WindowExt as _, v_flex};
use gpui_kit_assets::IconName;

pub type OnPick = Rc<dyn Fn(String, &mut Window, &mut App)>;

/// Case-insensitive filter: names starting with the query first, then
/// names containing it, each in their original order.
pub fn filter(all: &[String], query: &str) -> Vec<String> {
    let q = query.trim().to_lowercase();
    let (mut starts, mut contains): (Vec<_>, Vec<_>) = (vec![], vec![]);
    for name in all {
        let l = name.to_lowercase();
        if l.starts_with(&q) {
            starts.push(name.clone());
        } else if l.contains(&q) {
            contains.push(name.clone());
        }
    }
    starts.extend(contains);
    starts
}

pub struct DbPicker {
    all: Vec<String>,
    current: String,
    query: Entity<InputState>,
    highlighted: usize,
    on_pick: OnPick,
    _sub: Subscription,
}

impl DbPicker {
    pub fn open(
        all: Vec<String>,
        current: String,
        on_pick: OnPick,
        window: &mut Window,
        cx: &mut App,
    ) {
        let picker = cx.new(|cx| {
            let query = cx.new(|cx| InputState::new(window, cx).placeholder("Switch database…"));
            let sub = cx.subscribe_in(
                &query,
                window,
                // Enter is handled by the dialog's `on_ok` below, not here.
                |this: &mut Self, _, ev: &InputEvent, _, cx| {
                    if let InputEvent::Change = ev {
                        this.highlighted = 0;
                        cx.notify();
                    }
                },
            );
            // Start on the current database so Enter is a no-op by default.
            let highlighted = all.iter().position(|d| *d == current).unwrap_or(0);
            Self {
                all,
                current,
                query,
                highlighted,
                on_pick,
                _sub: sub,
            }
        });
        let query = picker.read(cx).query.clone();
        let first = std::cell::Cell::new(true);
        window.open_dialog(cx, move |dialog, window, cx| {
            // Focus once the dialog exists: opening it moves focus to the
            // dialog itself, so focusing earlier gets undone.
            if first.replace(false) {
                query.update(cx, |q, cx| q.focus(window, cx));
            }
            // Enter in the search box bubbles up to the dialog as "OK", whose
            // default just closes it, dropping the picker before it can act
            // (GitHub #11). So "OK" is what picks the highlighted database.
            let p = picker.clone();
            dialog
                .title("Switch Database")
                .w(px(420.))
                .child(picker.clone())
                .on_ok(move |_, window, cx| {
                    p.update(cx, |p, cx| p.choose(p.highlighted, window, cx));
                    true
                })
        });
    }

    fn matches(&self, cx: &App) -> Vec<String> {
        filter(&self.all, &self.query.read(cx).value())
    }

    /// Switches to match `ix` (clamped) unless it's already current. The
    /// caller closes the dialog.
    fn choose(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let matches = self.matches(cx);
        let Some(name) = matches
            .get(ix.min(matches.len().saturating_sub(1)))
            .cloned()
        else {
            return;
        };
        if name != self.current {
            (self.on_pick)(name, window, cx);
        }
    }

    fn on_key(&mut self, ev: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let n = self.matches(cx).len();
        if n == 0 {
            return;
        }
        match ev.keystroke.key.as_str() {
            "down" => self.highlighted = (self.highlighted + 1) % n,
            "up" => self.highlighted = (self.highlighted + n - 1) % n,
            _ => return,
        }
        cx.stop_propagation();
        cx.notify();
    }
}

impl Render for DbPicker {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let matches = self.matches(cx);
        let highlighted = self.highlighted.min(matches.len().saturating_sub(1));
        let theme = cx.theme().clone();
        v_flex()
            .gap_2()
            .capture_key_down(cx.listener(Self::on_key))
            .child(Input::new(&self.query).prefix(Icon::new(IconName::Database)))
            .child(
                div()
                    .id("db-picker-list")
                    .max_h(px(320.))
                    .overflow_y_scroll()
                    .when(matches.is_empty(), |d| {
                        d.p_2()
                            .text_sm()
                            .text_color(theme.muted_foreground)
                            .child("No matching databases")
                    })
                    .children(matches.into_iter().enumerate().map(|(i, name)| {
                        let is_current = name == self.current;
                        let selector = format!("db-row-{name}");
                        div()
                            .id(("db", i))
                            .debug_selector(|| selector)
                            .flex()
                            .items_center()
                            .gap_2()
                            .px_2()
                            .py_1()
                            .rounded_md()
                            .text_sm()
                            .cursor_pointer()
                            .when(i == highlighted, |d| {
                                d.bg(theme.accent).text_color(theme.accent_foreground)
                            })
                            .hover(|d| d.bg(theme.accent))
                            .child(div().flex_1().truncate().child(name))
                            .when(is_current, |d| {
                                d.child(Icon::new(IconName::Check).text_color(theme.primary))
                            })
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.choose(i, window, cx);
                                window.close_dialog(cx);
                            }))
                    })),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::filter;

    #[test]
    fn prefix_matches_first_case_insensitive() {
        let all: Vec<String> = ["postgres", "app_dev", "App", "analytics", "template1"]
            .map(String::from)
            .to_vec();
        assert_eq!(filter(&all, "app"), ["app_dev", "App"]);
        assert_eq!(
            filter(&all, "a"),
            ["app_dev", "App", "analytics", "template1"]
        );
        assert_eq!(filter(&all, "  TEMP "), ["template1"]);
        assert_eq!(filter(&all, "").len(), 5);
        assert!(filter(&all, "zzz").is_empty());
    }
}
