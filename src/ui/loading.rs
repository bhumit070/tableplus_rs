//! Loading states (design.md, screen 22): skeleton rows that appear only
//! after a short delay (a fast load never flashes), and a small chip with
//! what's loading, how long it has taken and Stop. Views that show either
//! call `tick` so the elapsed time and the delayed skeleton repaint.

use std::time::{Duration, Instant};

use gpui::{
    Animation, AnimationExt as _, AnyElement, App, Context, Div, ElementId, IntoElement,
    ParentElement as _, SharedString, Styled as _, Window, div, prelude::*, pulsating_between,
    relative,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::notification::Notification;
use gpui_component::spinner::Spinner;
use gpui_component::{Sizable as _, WindowExt as _, h_flex, v_flex};

use super::theme::{dp, tokens};

/// Skeletons wait this long, so a quick load shows nothing at all.
pub const DELAY: Duration = Duration::from_millis(150);

/// Whether a load that began at `since` has run long enough to show.
pub fn visible(since: Option<Instant>) -> bool {
    since.is_some_and(|s| s.elapsed() >= DELAY)
}

/// Repaints the view every 100 ms while `busy` says so, for the elapsed
/// time and the delayed skeleton. Stops by itself.
pub fn tick<V: 'static>(cx: &mut Context<V>, busy: fn(&V) -> bool) {
    cx.spawn(async move |this, cx| {
        loop {
            cx.background_executor()
                .timer(Duration::from_millis(100))
                .await;
            let go_on = this
                .update(cx, |v, cx| {
                    cx.notify();
                    busy(v)
                })
                .unwrap_or(false);
            if !go_on {
                break;
            }
        }
    })
    .detach();
}

/// "0.4 s", "12 s", "2 min 05 s".
pub fn elapsed(since: Instant) -> String {
    let s = since.elapsed().as_secs_f32();
    if s < 10. {
        format!("{s:.1} s")
    } else if s < 60. {
        format!("{s:.0} s")
    } else {
        let s = s as u64;
        format!("{} min {:02} s", s / 60, s % 60)
    }
}

/// `rows` grey placeholder rows of `cols` cells, gently pulsing; for grids.
pub fn skeleton_rows(id: impl Into<ElementId>, cols: usize, rows: usize, cx: &App) -> AnyElement {
    let t = tokens(cx);
    let cols = cols.clamp(1, 8);
    // Fixed, varied widths: looks like data, never jumps between frames.
    const WIDTHS: [f32; 7] = [0.55, 0.8, 0.4, 0.7, 0.6, 0.85, 0.45];
    v_flex()
        .size_full()
        .overflow_hidden()
        .children((0..rows).map(|r| {
            h_flex()
                .h(dp(32.))
                .flex_none()
                .px(dp(12.))
                .gap(dp(16.))
                .border_b_1()
                .border_color(t.border.opacity(0.5))
                .children((0..cols).map(|c| {
                    let w = WIDTHS[(r * 3 + c * 5) % WIDTHS.len()];
                    div()
                        .flex_1()
                        .child(div().h(dp(10.)).w(relative(w)).rounded(dp(4.)).bg(t.hover))
                }))
        }))
        .with_animation(
            id,
            Animation::new(Duration::from_millis(1200))
                .repeat()
                .with_easing(pulsating_between(0.45, 1.0)),
            |el, delta| el.opacity(delta),
        )
        .into_any_element()
}

/// Sidebar-sized placeholder rows (icon + name).
pub fn skeleton_list(id: impl Into<ElementId>, rows: usize, cx: &App) -> AnyElement {
    let t = tokens(cx);
    const WIDTHS: [f32; 5] = [0.6, 0.45, 0.75, 0.5, 0.65];
    v_flex()
        .px(dp(12.))
        .pt(dp(6.))
        .gap(dp(14.))
        .children((0..rows).map(|r| {
            h_flex()
                .gap(dp(10.))
                .child(div().size(dp(12.)).rounded(dp(3.)).bg(t.hover))
                .child(
                    div()
                        .h(dp(10.))
                        .w(relative(WIDTHS[r % WIDTHS.len()]))
                        .rounded(dp(4.))
                        .bg(t.hover),
                )
        }))
        .with_animation(
            id,
            Animation::new(Duration::from_millis(1200))
                .repeat()
                .with_easing(pulsating_between(0.45, 1.0)),
            |el, delta| el.opacity(delta),
        )
        .into_any_element()
}

/// The "Loading orders · 1.2 s · Stop" chip, centred above the bottom of the
/// panel it's placed in (as an absolutely positioned child).
pub fn chip(label: impl Into<SharedString>, since: Instant, stop: Option<Button>, cx: &App) -> Div {
    let t = tokens(cx);
    div()
        .absolute()
        .bottom(dp(18.))
        .left_0()
        .right_0()
        .flex()
        .justify_center()
        .child(
            h_flex()
                .debug_selector(|| "loading-chip".into())
                .h(dp(32.))
                .pl(dp(12.))
                .pr(dp(if stop.is_some() { 4. } else { 12. }))
                .gap(dp(8.))
                .rounded(dp(10.))
                .border_1()
                .border_color(t.border)
                .bg(t.raised)
                .shadow_md()
                .text_size(dp(12.))
                .text_color(t.text_secondary)
                .child(Spinner::new().xsmall())
                .child(label.into())
                .child(div().text_color(t.text_faint).child("·"))
                .child(div().text_color(t.text_muted).child(elapsed(since)))
                .children(stop.map(|b| b.ghost().xsmall())),
        )
}

/// A centred message for a panel that waits on something it can't show
/// rows for yet (structure, DDL): spinner and a line of text, after the
/// delay.
pub fn waiting(label: impl Into<SharedString>, since: Option<Instant>, cx: &App) -> AnyElement {
    let t = tokens(cx);
    if !visible(since) {
        return div().size_full().into_any_element();
    }
    h_flex()
        .size_full()
        .justify_center()
        .items_center()
        .gap(dp(8.))
        .text_size(dp(12.5))
        .text_color(t.text_muted)
        .child(Spinner::new().small())
        .child(label.into())
        .into_any_element()
}

/// Key type for the toasts `busy_toast` shows.
struct BusyToast;

/// A toast with a spinner that stays until `end_busy` (long jobs such as a
/// backup), instead of a note that fades while the work goes on.
pub fn busy_toast(key: &str, text: impl Into<SharedString>, window: &mut Window, cx: &mut App) {
    let text: SharedString = text.into();
    let note = Notification::new()
        .id1::<BusyToast>(SharedString::from(key.to_string()))
        .autohide(false)
        .content(move |_, _, cx| {
            h_flex()
                .gap(dp(10.))
                .text_size(dp(12.5))
                .text_color(tokens(cx).text)
                .child(Spinner::new().small())
                .child(text.clone())
                .into_any_element()
        });
    window.push_notification(note, cx);
}

/// Removes the toast `busy_toast` showed under `key`.
pub fn end_busy(key: &str, window: &mut Window, cx: &mut App) {
    window.remove_notification1::<BusyToast>(SharedString::from(key.to_string()), cx);
}
