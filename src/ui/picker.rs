//! Searchable pick-one dialog (⌘K databases, connections): type to filter,
//! ↑/↓ to move, Enter or click to pick.

use std::rc::Rc;

use gpui::{
    App, Context, Entity, Hsla, IntoElement, KeyDownEvent, ParentElement, Render, SharedString,
    Styled, Subscription, Window, div, prelude::*, px,
};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::{ActiveTheme as _, Icon, WindowExt as _, h_flex, v_flex};
use gpui_kit_assets::IconName;

/// Called with the picked item's id.
pub type OnPick = Rc<dyn Fn(String, &mut Window, &mut App)>;

#[derive(Clone, Debug, PartialEq)]
pub struct PickItem {
    pub id: String,
    pub label: String,
    /// Secondary text, e.g. "open · PostgreSQL" or a host.
    pub detail: Option<String>,
    /// Colored dot (connection tag).
    pub dot: Option<Hsla>,
    /// Shown with a check; picking it again does nothing.
    pub current: bool,
}

impl PickItem {
    pub fn new(id: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            detail: None,
            dot: None,
            current: false,
        }
    }
}

/// Case-insensitive: labels starting with the query first, then labels or
/// details containing it, each in their original order.
pub fn filter(all: &[PickItem], query: &str) -> Vec<PickItem> {
    let q = query.trim().to_lowercase();
    let (mut starts, mut contains): (Vec<_>, Vec<_>) = (vec![], vec![]);
    for item in all {
        let label = item.label.to_lowercase();
        let detail = item.detail.as_deref().unwrap_or("").to_lowercase();
        if label.starts_with(&q) {
            starts.push(item.clone());
        } else if label.contains(&q) || detail.contains(&q) {
            contains.push(item.clone());
        }
    }
    starts.extend(contains);
    starts
}

pub struct Picker {
    all: Vec<PickItem>,
    query: Entity<InputState>,
    highlighted: usize,
    on_pick: OnPick,
    icon: IconName,
    _sub: Subscription,
}

impl Picker {
    pub fn open(
        title: impl Into<SharedString>,
        placeholder: &str,
        icon: IconName,
        all: Vec<PickItem>,
        on_pick: OnPick,
        window: &mut Window,
        cx: &mut App,
    ) {
        let placeholder = placeholder.to_string();
        let picker = cx.new(|cx| {
            let query = cx.new(|cx| InputState::new(window, cx).placeholder(placeholder));
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
            // Start on the current item so Enter is a no-op by default.
            let highlighted = all.iter().position(|i| i.current).unwrap_or(0);
            Self {
                all,
                query,
                highlighted,
                on_pick,
                icon,
                _sub: sub,
            }
        });
        let query = picker.read(cx).query.clone();
        let first = std::cell::Cell::new(true);
        let title = title.into();
        window.open_dialog(cx, move |dialog, window, cx| {
            // Focus once the dialog exists: opening it moves focus to the
            // dialog itself, so focusing earlier gets undone.
            if first.replace(false) {
                query.update(cx, |q, cx| q.focus(window, cx));
            }
            // Enter in the search box bubbles up to the dialog as "OK", whose
            // default just closes it, dropping the picker before it can act
            // (GitHub #11). So "OK" is what picks the highlighted item.
            let p = picker.clone();
            dialog
                .title(title.clone())
                .w(px(460.))
                .child(picker.clone())
                .on_ok(move |_, window, cx| {
                    p.update(cx, |p, cx| p.choose(p.highlighted, window, cx));
                    // `choose` already closed this dialog.
                    false
                })
        });
    }

    fn matches(&self, cx: &App) -> Vec<PickItem> {
        filter(&self.all, &self.query.read(cx).value())
    }

    /// Closes the dialog, then picks match `ix` (clamped) unless it's
    /// already current. Closing first matters: the pick may open another
    /// dialog (e.g. the connection form), and closing afterwards would close
    /// that one instead.
    fn choose(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let matches = self.matches(cx);
        let item = matches
            .get(ix.min(matches.len().saturating_sub(1)))
            .cloned();
        window.close_dialog(cx);
        if let Some(item) = item.filter(|i| !i.current) {
            (self.on_pick)(item.id, window, cx);
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

impl Render for Picker {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let matches = self.matches(cx);
        let highlighted = self.highlighted.min(matches.len().saturating_sub(1));
        let theme = cx.theme().clone();
        v_flex()
            .gap_2()
            .capture_key_down(cx.listener(Self::on_key))
            .child(Input::new(&self.query).prefix(Icon::new(self.icon)))
            .child(
                div()
                    .id("picker-list")
                    .max_h(px(360.))
                    .overflow_y_scroll()
                    .when(matches.is_empty(), |d| {
                        d.p_2()
                            .text_sm()
                            .text_color(theme.muted_foreground)
                            .child("No matches")
                    })
                    .children(matches.into_iter().enumerate().map(|(i, item)| {
                        let selector = format!("pick-row-{}", item.id);
                        let on = i == highlighted;
                        h_flex()
                            .id(("pick", i))
                            .debug_selector(|| selector)
                            .gap_2()
                            .px_2()
                            .py_1()
                            .rounded_md()
                            .text_sm()
                            .cursor_pointer()
                            .when(on, |d| {
                                d.bg(theme.accent).text_color(theme.accent_foreground)
                            })
                            .hover(|d| d.bg(theme.accent))
                            .when_some(item.dot, |d, c| {
                                d.child(div().size(px(8.)).rounded_full().bg(c))
                            })
                            .child(div().flex_1().min_w_0().truncate().child(item.label))
                            .when_some(item.detail, |d, t| {
                                d.child(
                                    div()
                                        .text_xs()
                                        .text_color(theme.muted_foreground)
                                        .truncate()
                                        .child(t),
                                )
                            })
                            .when(item.current, |d| {
                                d.child(Icon::new(IconName::Check).text_color(theme.primary))
                            })
                            .on_click(
                                cx.listener(move |this, _, window, cx| this.choose(i, window, cx)),
                            )
                    })),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn items(labels: &[&str]) -> Vec<PickItem> {
        labels.iter().map(|l| PickItem::new(*l, *l)).collect()
    }

    fn labels(v: Vec<PickItem>) -> Vec<String> {
        v.into_iter().map(|i| i.label).collect()
    }

    #[test]
    fn prefix_matches_first_case_insensitive() {
        let all = items(&["postgres", "app_dev", "App", "analytics", "template1"]);
        assert_eq!(labels(filter(&all, "app")), ["app_dev", "App"]);
        assert_eq!(
            labels(filter(&all, "a")),
            ["app_dev", "App", "analytics", "template1"]
        );
        assert_eq!(labels(filter(&all, "  TEMP ")), ["template1"]);
        assert_eq!(filter(&all, "").len(), 5);
        assert!(filter(&all, "zzz").is_empty());
    }

    #[test]
    fn details_are_searchable_after_labels() {
        let mut prod = PickItem::new("c:1", "Orders");
        prod.detail = Some("PostgreSQL · db.prod.example".into());
        let all = vec![prod, PickItem::new("c:2", "prod-mirror")];
        assert_eq!(labels(filter(&all, "prod")), ["prod-mirror", "Orders"]);
    }
}
