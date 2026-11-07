//! Searchable pick-one dialog in the command-palette look (design.md,
//! screens 05 / 17): ⌘P go to, ⌘K databases, connections. Type to filter
//! (matches in bold accent), ↑/↓ to move, Enter or click to pick. Items can
//! carry a section, an icon and a shortcut.

use std::rc::Rc;

use std::cell::Cell;

use gpui::{
    AnyElement, App, Context, Entity, FontWeight, Hsla, IntoElement, KeyDownEvent, ParentElement,
    Render, SharedString, Styled, Subscription, Window, div, prelude::*, px,
};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::{Icon, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit_assets::IconName;

use super::dialog;
use super::theme::{caps, dp, kbd, tokens};

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
    /// Section heading ("Open now", "Tables"), shown while not searching.
    pub group: Option<SharedString>,
    pub icon: Option<IconName>,
    /// A shortcut shown as a key cap on the right ("⌘1").
    pub hint: Option<String>,
}

impl PickItem {
    pub fn new(id: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            detail: None,
            dot: None,
            current: false,
            group: None,
            icon: None,
            hint: None,
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
    title: SharedString,
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
                title: title.into(),
                _sub: sub,
            }
        });
        let query = picker.read(cx).query.clone();
        let first = Cell::new(true);
        let w = dialog::width(640., cx);
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
            dialog::shell(dialog, w, cx)
                .margin_top(px(96.))
                .pt(dp(6.))
                .pb(dp(6.))
                .px(dp(6.))
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

/// `label` with the first case-insensitive match of `q` in bold accent.
fn highlighted_label(label: &str, q: &str, cx: &App) -> AnyElement {
    let t = tokens(cx);
    let lower = label.to_lowercase();
    let q = q.trim().to_lowercase();
    match (!q.is_empty()).then(|| lower.find(&q)).flatten() {
        // Byte offsets line up only when lowercasing kept the length.
        Some(at) if lower.len() == label.len() => h_flex()
            .min_w_0()
            .child(label[..at].to_string())
            .child(
                div()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(t.accent)
                    .child(label[at..at + q.len()].to_string()),
            )
            .child(label[at + q.len()..].to_string())
            .into_any_element(),
        _ => div().truncate().child(label.to_string()).into_any_element(),
    }
}

impl Render for Picker {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = tokens(cx);
        let matches = self.matches(cx);
        let highlighted = self.highlighted.min(matches.len().saturating_sub(1));
        let q = self.query.read(cx).value().to_string();
        let searching = !q.trim().is_empty();
        let mut rows: Vec<AnyElement> = vec![];
        let mut last_group: Option<SharedString> = None;
        for (i, item) in matches.into_iter().enumerate() {
            if !searching && item.group.is_some() && item.group != last_group {
                last_group = item.group.clone();
                rows.push(
                    div()
                        .px(dp(10.))
                        .pt(dp(if i == 0 { 4. } else { 12. }))
                        .pb(dp(4.))
                        .child(caps(item.group.as_deref().unwrap_or_default(), cx))
                        .into_any_element(),
                );
            }
            let selector = format!("pick-row-{}", item.id);
            let on = i == highlighted;
            rows.push(
                h_flex()
                    .id(("pick", i))
                    .debug_selector(|| selector)
                    .h(dp(34.))
                    .gap(dp(10.))
                    .px(dp(10.))
                    .rounded(dp(7.))
                    .text_size(dp(13.))
                    .text_color(t.text)
                    .cursor_pointer()
                    .when(on, |d| d.bg(t.accent.opacity(0.16)))
                    .when(!on, |d| d.hover(|d| d.bg(t.hover)))
                    .when_some(item.icon, |d, icon| {
                        d.child(Icon::new(icon).small().text_color(if on {
                            t.accent
                        } else {
                            t.text_muted
                        }))
                    })
                    .when_some(item.dot, |d, c| {
                        d.child(div().flex_none().size(dp(8.)).rounded_full().bg(c))
                    })
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .child(highlighted_label(&item.label, &q, cx)),
                    )
                    .when_some(item.detail, |d, detail| {
                        d.child(
                            div()
                                .flex_none()
                                .max_w(dp(260.))
                                .truncate()
                                .text_size(dp(11.5))
                                .text_color(t.text_muted)
                                .child(detail),
                        )
                    })
                    .when_some(item.hint, |d, k| d.child(kbd(k, cx)))
                    .when(item.current, |d| {
                        d.child(Icon::new(IconName::Check).small().text_color(t.accent))
                    })
                    .on_click(cx.listener(move |this, _, window, cx| this.choose(i, window, cx)))
                    .into_any_element(),
            );
        }
        let empty = rows.is_empty();
        v_flex()
            .capture_key_down(cx.listener(Self::on_key))
            .child(
                h_flex()
                    .h(dp(46.))
                    .px(dp(8.))
                    .gap(dp(4.))
                    .border_b_1()
                    .border_color(t.border)
                    .child(
                        Input::new(&self.query)
                            .appearance(false)
                            .prefix(Icon::new(self.icon).text_color(t.text_muted)),
                    ),
            )
            .child(
                div()
                    .id("picker-list")
                    .max_h(dp(380.))
                    .overflow_y_scroll()
                    .py(dp(6.))
                    .when(empty, |d| {
                        d.child(
                            div()
                                .px(dp(12.))
                                .py(dp(10.))
                                .text_size(dp(12.5))
                                .text_color(t.text_muted)
                                .child("No matches"),
                        )
                    })
                    .children(rows),
            )
            .child(
                h_flex()
                    .h(dp(32.))
                    .px(dp(10.))
                    .gap(dp(12.))
                    .border_t_1()
                    .border_color(t.border)
                    .text_size(dp(11.))
                    .text_color(t.text_faint)
                    .child(caps(&self.title, cx))
                    .child(div().flex_1())
                    .child(
                        h_flex()
                            .gap(dp(4.))
                            .child(kbd("↑", cx))
                            .child(kbd("↓", cx))
                            .child("move"),
                    )
                    .child(h_flex().gap(dp(4.)).child(kbd("↵", cx)).child("open"))
                    .child(h_flex().gap(dp(4.)).child(kbd("esc", cx)).child("close")),
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
