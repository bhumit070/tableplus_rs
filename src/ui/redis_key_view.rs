//! The Redis key editor (design.md screen 19), beside the key list: a
//! header (type badge, key, size, a live TTL countdown, Rename, Delete) and
//! a type-specific editor: hash field / value, list index / value, set
//! members, zset member / score, string text (JSON formatted), stream read
//! only. Edits stage with the grid's colours and run on Save in one MULTI.

use std::rc::Rc;
use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use gpui::{
    AnyElement, App, AppContext as _, Context, Entity, EntityId, FontWeight, Hsla, IntoElement,
    ParentElement, Render, Styled, Subscription, Window, div, prelude::*, transparent_black,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::{Editor, EditorState, Input, InputEvent, InputState};
use gpui_component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_component::notification::Notification;
use gpui_component::{ActiveTheme as _, Icon, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit_assets::IconName;

use super::confirm::ConfirmWrite;
use super::grid::human_size;
use super::row_panel::pretty_json;
use super::theme::{Tokens, dp, tokens};
use super::{Shared, prompt_text};
use crate::db::redis_key::{KEY_ITEMS_CAP, KeyDetail, KeyEdit, preview};
use crate::runtime;
use serde_json::{Value as JsonValue, from_str};

/// Called after anything changed on the server, so the key list reloads.
pub type OnChanged = Rc<dyn Fn(&mut Window, &mut App)>;

struct ItemRow {
    /// As read from the server; `None` for one added here.
    original: Option<(String, String)>,
    first: Entity<InputState>,
    second: Entity<InputState>,
    removed: bool,
}

/// How a row differs from what the server has.
#[derive(Clone, Copy, PartialEq, Eq)]
enum RowState {
    Unchanged,
    Altered,
    New,
    Removed,
}

pub struct RedisKeyView {
    shared: Rc<Shared>,
    key: Option<String>,
    detail: Option<KeyDetail>,
    /// When the detail was read: the TTL counts down from there.
    read_at: Instant,
    load_seq: u64,
    loading: bool,
    error: Option<String>,
    rows: Vec<ItemRow>,
    /// The string editor; JSON is shown formatted.
    text: Entity<EditorState>,
    original_text: String,
    /// The string was compact JSON shown formatted: Save compacts it again.
    compact_json: bool,
    show_preview: bool,
    saving: bool,
    on_changed: OnChanged,
    _subscriptions: Vec<Subscription>,
}

/// hash blue, string green, list amber, set violet, zset teal, stream red.
pub fn type_colour(kind: &str, t: &Tokens) -> Hsla {
    match kind {
        "hash" => t.info,
        "string" => t.success,
        "list" => t.warning,
        "set" => t.keyword,
        "zset" => t.number,
        "stream" => t.danger,
        _ => t.text_muted,
    }
}

/// "4m 12s", "2h 05m", "3d 4h".
fn countdown(seconds: i64) -> String {
    let seconds = seconds.max(0);
    match seconds {
        0..60 => format!("{seconds}s"),
        60..3600 => format!("{}m {:02}s", seconds / 60, seconds % 60),
        3600..86400 => format!("{}h {:02}m", seconds / 3600, seconds % 3600 / 60),
        _ => format!("{}d {}h", seconds / 86400, seconds % 86400 / 3600),
    }
}

impl RedisKeyView {
    pub fn new(
        shared: Rc<Shared>,
        on_changed: OnChanged,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let text = cx.new(|cx| EditorState::new(window, cx).soft_wrap(true));
        let subscriptions = vec![cx.observe(&text, |_, _, cx| cx.notify())];
        // The TTL countdown ticks once a second while a key expires.
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                let alive = this
                    .update(cx, |view, cx| {
                        if view
                            .detail
                            .as_ref()
                            .is_some_and(|detail| detail.ttl.is_some())
                        {
                            cx.notify();
                        }
                    })
                    .is_ok();
                if !alive {
                    break;
                }
            }
        })
        .detach();
        RedisKeyView {
            shared,
            key: None,
            detail: None,
            read_at: Instant::now(),
            load_seq: 0,
            loading: false,
            error: None,
            rows: vec![],
            text,
            original_text: String::new(),
            compact_json: false,
            show_preview: false,
            saving: false,
            on_changed,
            _subscriptions: subscriptions,
        }
    }

    /// Shows `key` (reading it if it isn't the one shown). Staged edits of
    /// the previous key are dropped, as moving to another row would.
    pub fn show_key(&mut self, key: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        if key == self.key {
            return;
        }
        self.key = key;
        self.reload(window, cx);
    }

    #[cfg(test)]
    pub fn shown_key(&self) -> Option<&str> {
        self.key.as_deref()
    }

    pub fn reload(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.load_seq += 1;
        let seq = self.load_seq;
        self.error = None;
        self.show_preview = false;
        let Some(key) = self.key.clone() else {
            self.detail = None;
            self.rows.clear();
            self.loading = false;
            cx.notify();
            return;
        };
        self.loading = true;
        cx.notify();
        let conn = self.shared.conn.clone();
        let task = runtime::spawn(async move { conn.read().await.key_detail(&key).await });
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            this.update_in(cx, |view, window, cx| {
                if view.load_seq != seq {
                    return;
                }
                view.loading = false;
                match result {
                    Ok(detail) => view.set_detail(detail, window, cx),
                    Err(error) => {
                        view.shared.report(&format!("{error:#}"));
                        view.error = Some(format!("{error:#}"));
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn set_detail(&mut self, detail: KeyDetail, window: &mut Window, cx: &mut Context<Self>) {
        self.rows.clear();
        self._subscriptions.truncate(1);
        if detail.kind == "string" {
            let raw = detail
                .items
                .first()
                .map(|(_, text)| text.clone())
                .unwrap_or_default();
            let formatted = pretty_json(&raw);
            self.compact_json = formatted.as_ref().is_some_and(|pretty| *pretty != raw);
            let shown = formatted.unwrap_or(raw);
            self.original_text = shown.clone();
            // A fresh editor: the language is set when it's made.
            let json = shown.trim_start().starts_with(['{', '[']);
            self.text = cx.new(|cx| {
                let editor = EditorState::new(window, cx)
                    .soft_wrap(true)
                    .line_number(json)
                    .default_value(shown);
                if json {
                    editor.language("json")
                } else {
                    editor
                }
            });
            self._subscriptions[0] = cx.observe(&self.text, |_, _, cx| cx.notify());
        } else {
            for item in detail.items.clone() {
                self.push_row(Some(item), window, cx);
            }
        }
        self.read_at = Instant::now();
        self.detail = Some(detail);
    }

    fn push_row(
        &mut self,
        original: Option<(String, String)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let kind = self.kind();
        let (first_value, second_value) = original.clone().unwrap_or_default();
        let (first_placeholder, second_placeholder) = match kind.as_str() {
            "hash" => ("field", "value"),
            "list" => ("new", "value"),
            "zset" => ("member", "score"),
            _ => ("member", ""),
        };
        let mut input = |value: String, placeholder: &str| {
            let placeholder = placeholder.to_string();
            let input = cx.new(|cx| {
                InputState::new(window, cx)
                    .default_value(value)
                    .placeholder(placeholder)
            });
            self._subscriptions
                .push(cx.subscribe(&input, |_, _, event, cx| {
                    if let InputEvent::Change = event {
                        cx.notify();
                    }
                }));
            input
        };
        let row = ItemRow {
            first: input(first_value, first_placeholder),
            second: input(
                if kind == "zset" && original.is_none() {
                    "0".into()
                } else {
                    second_value
                },
                second_placeholder,
            ),
            removed: false,
            original,
        };
        self.rows.push(row);
    }

    fn kind(&self) -> String {
        self.detail
            .as_ref()
            .map(|detail| detail.kind.clone())
            .unwrap_or_default()
    }

    fn values(&self, row: &ItemRow, cx: &App) -> (String, String) {
        (
            row.first.read(cx).value().to_string(),
            row.second.read(cx).value().to_string(),
        )
    }

    fn row_state(&self, row: &ItemRow, cx: &App) -> RowState {
        match &row.original {
            None => RowState::New,
            Some(_) if row.removed => RowState::Removed,
            Some(original) => {
                let now = self.values(row, cx);
                // A list row's first column is its index, never edited.
                let same = if self.kind() == "list" {
                    now.1 == original.1
                } else {
                    now == *original
                };
                if same {
                    RowState::Unchanged
                } else {
                    RowState::Altered
                }
            }
        }
    }

    /// What Save would send.
    fn edits(&self, cx: &App) -> Result<Vec<KeyEdit>> {
        let kind = self.kind();
        if kind == "string" {
            let text = self.text.read(cx).value().to_string();
            if text == self.original_text {
                return Ok(vec![]);
            }
            let stored = match from_str::<JsonValue>(&text) {
                Ok(json) if self.compact_json => json.to_string(),
                _ => text,
            };
            return Ok(vec![KeyEdit::SetString(stored)]);
        }
        let mut edits = vec![];
        for row in &self.rows {
            let state = self.row_state(row, cx);
            let (first, second) = self.values(row, cx);
            if state == RowState::New && first.trim().is_empty() && kind != "list" {
                continue;
            }
            let score = |text: &str| -> Result<f64> {
                match text.trim().parse::<f64>() {
                    Ok(score) => Ok(score),
                    Err(_) => bail!("{text:?} isn't a number; zset scores are numbers"),
                }
            };
            let original = row.original.clone().unwrap_or_default();
            match (kind.as_str(), state) {
                (_, RowState::Unchanged) => {}
                ("hash", RowState::Removed) => edits.push(KeyEdit::HashDelete(original.0)),
                ("hash", RowState::New) => edits.push(KeyEdit::HashSet {
                    field: first,
                    value: second,
                }),
                ("hash", RowState::Altered) => {
                    if first != original.0 {
                        edits.push(KeyEdit::HashDelete(original.0));
                    }
                    edits.push(KeyEdit::HashSet {
                        field: first,
                        value: second,
                    });
                }
                ("list", RowState::Removed) => {
                    edits.push(KeyEdit::ListRemove(original.0.parse().unwrap_or_default()))
                }
                ("list", RowState::New) => edits.push(KeyEdit::ListPush(second)),
                ("list", RowState::Altered) => edits.push(KeyEdit::ListSet {
                    index: original.0.parse().unwrap_or_default(),
                    value: second,
                }),
                ("set", RowState::Removed) => edits.push(KeyEdit::SetRemove(original.0)),
                ("set", RowState::New) => edits.push(KeyEdit::SetAdd(first)),
                ("set", RowState::Altered) => {
                    edits.push(KeyEdit::SetRemove(original.0));
                    edits.push(KeyEdit::SetAdd(first));
                }
                ("zset", RowState::Removed) => edits.push(KeyEdit::ZsetRemove(original.0)),
                ("zset", RowState::New) => edits.push(KeyEdit::ZsetAdd {
                    member: first,
                    score: score(&second)?,
                }),
                ("zset", RowState::Altered) => {
                    if first != original.0 {
                        edits.push(KeyEdit::ZsetRemove(original.0));
                    }
                    edits.push(KeyEdit::ZsetAdd {
                        member: first,
                        score: score(&second)?,
                    });
                }
                _ => {}
            }
        }
        Ok(edits)
    }

    /// Staged rows (or the edited string), for the bar.
    pub fn change_count(&self, cx: &App) -> usize {
        if self.kind() == "string" {
            return usize::from(self.text.read(cx).value().as_ref() != self.original_text);
        }
        self.rows
            .iter()
            .filter(|row| match self.row_state(row, cx) {
                RowState::Unchanged => false,
                RowState::New => {
                    let (first, second) = self.values(row, cx);
                    !first.trim().is_empty() || (self.kind() == "list" && !second.is_empty())
                }
                _ => true,
            })
            .count()
    }

    pub fn discard(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(detail) = self.detail.clone() {
            self.set_detail(detail, window, cx);
        }
        self.show_preview = false;
        self.error = None;
        cx.notify();
    }

    pub fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(key) = self.key.clone() else { return };
        if self.saving {
            return;
        }
        let edits = match self.edits(cx) {
            Ok(edits) if edits.is_empty() => return,
            Ok(edits) => edits,
            Err(error) => {
                self.error = Some(format!("{error:#}"));
                cx.notify();
                return;
            }
        };
        let gate = crate::db::safety::gate_write(&self.shared.cfg);
        let view = cx.entity().downgrade();
        let confirmed = edits.clone();
        if super::guarded(
            gate,
            &self.shared.cfg,
            "Save",
            preview(&key, &edits),
            window,
            cx,
            move |window, cx| {
                view.update(cx, |view, cx| view.run_edits(confirmed.clone(), window, cx))
                    .ok();
            },
        ) {
            self.run_edits(edits, window, cx);
        }
    }

    fn run_edits(&mut self, edits: Vec<KeyEdit>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(key) = self.key.clone() else { return };
        self.saving = true;
        self.error = None;
        cx.notify();
        let conn = self.shared.conn.clone();
        let task =
            runtime::spawn(async move { conn.read().await.apply_key_edits(&key, &edits).await });
        self.after(task, "Saved", window, cx);
    }

    /// Waits for `task`; then reads the key again and tells the list.
    fn after(
        &mut self,
        task: impl Future<Output = Result<()>> + 'static,
        success: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let on_changed = self.on_changed.clone();
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            this.update_in(cx, |view, window, cx| {
                view.saving = false;
                match result {
                    Ok(()) => {
                        window.push_notification(Notification::success(success), cx);
                        view.reload(window, cx);
                        on_changed(window, cx);
                    }
                    Err(error) => view.error = Some(format!("{error:#}")),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn set_ttl(&mut self, seconds: Option<i64>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(key) = self.key.clone() else { return };
        let conn = self.shared.conn.clone();
        let task = runtime::spawn(async move { conn.read().await.set_ttl(&key, seconds).await });
        self.after(
            task,
            if seconds.is_some() {
                "Expiry set"
            } else {
                "Expiry removed"
            },
            window,
            cx,
        );
    }

    fn rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(old_key) = self.key.clone() else {
            return;
        };
        let view = cx.entity().downgrade();
        prompt_text(
            "Rename key",
            "",
            &old_key.clone(),
            window,
            cx,
            move |new_key, window, cx| {
                if new_key == old_key {
                    return true;
                }
                let (conn, from, to) = (
                    view.upgrade().map(|view| view.read(cx).shared.conn.clone()),
                    old_key.clone(),
                    new_key.clone(),
                );
                let Some(conn) = conn else { return true };
                let task =
                    runtime::spawn(async move { conn.read().await.rename_key(&from, &to).await });
                view.update(cx, |view, cx| {
                    view.key = Some(new_key);
                    view.after(task, "Renamed", window, cx);
                })
                .ok();
                true
            },
        );
    }

    fn delete(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(key) = self.key.clone() else { return };
        let view = cx.entity().downgrade();
        let cfg = self.shared.cfg.clone();
        ConfirmWrite::open(
            &cfg,
            "Delete key",
            format!("DEL {key:?}"),
            window,
            cx,
            Rc::new(move |window, cx| {
                let key = key.clone();
                view.update(cx, |view, cx| {
                    let conn = view.shared.conn.clone();
                    let task =
                        runtime::spawn(async move { conn.read().await.delete_key(&key).await });
                    view.after(task, "Key deleted", window, cx);
                })
                .ok();
            }),
        );
    }

    fn row_mut(&mut self, key: EntityId) -> Option<&mut ItemRow> {
        self.rows
            .iter_mut()
            .find(|row| row.first.entity_id() == key)
    }

    #[cfg(test)]
    pub fn detail_for_test(&self) -> Option<&KeyDetail> {
        self.detail.as_ref()
    }

    #[cfg(test)]
    pub fn edit_row_for_test(
        &mut self,
        index: usize,
        first: &str,
        second: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        while self.rows.len() <= index {
            self.push_row(None, window, cx);
        }
        let (first_input, second_input) = (
            self.rows[index].first.clone(),
            self.rows[index].second.clone(),
        );
        first_input.update(cx, |input, cx| {
            input.set_value(first.to_string(), window, cx)
        });
        second_input.update(cx, |input, cx| {
            input.set_value(second.to_string(), window, cx)
        });
    }

    #[cfg(test)]
    pub fn show_preview_for_test(&mut self, cx: &mut Context<Self>) {
        self.show_preview = true;
        cx.notify();
    }

    fn render_header(&self, detail: &KeyDetail, cx: &mut Context<Self>) -> AnyElement {
        let t = tokens(cx);
        let mono = cx.theme().mono_font_family.clone();
        let colour = type_colour(&detail.kind, &t);
        let unit = match detail.kind.as_str() {
            "string" => "chars",
            "hash" => "fields",
            "stream" => "entries",
            "set" | "zset" => "members",
            _ => "items",
        };
        let mut facts = vec![format!("{} {unit}", detail.length)];
        if let Some(size) = detail.size {
            facts.push(human_size(size as usize));
        }
        let left = detail
            .ttl
            .map(|ttl| ttl - self.read_at.elapsed().as_secs() as i64);
        let ttl_label = match left {
            Some(seconds) if seconds <= 0 => "expired".to_string(),
            Some(seconds) => format!("expires in {}", countdown(seconds)),
            None => "no expiry".to_string(),
        };
        let view = cx.entity();
        v_flex()
            .gap(dp(8.))
            .px(dp(14.))
            .py(dp(12.))
            .border_b_1()
            .border_color(t.border)
            .child(
                h_flex()
                    .gap(dp(8.))
                    .child(
                        div()
                            .px(dp(6.))
                            .py(dp(1.))
                            .rounded(dp(4.))
                            .bg(colour.opacity(0.14))
                            .text_color(colour)
                            .text_size(dp(11.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(detail.kind.to_uppercase()),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .font_family(mono)
                            .text_size(dp(13.5))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(detail.key.clone()),
                    )
                    .child(
                        Button::new("rename-key")
                            .cursor_pointer()
                            .icon(IconName::Pencil)
                            .ghost()
                            .xsmall()
                            .tooltip("Rename…")
                            .on_click(cx.listener(|view, _, window, cx| view.rename(window, cx))),
                    )
                    .child(
                        Button::new("delete-key")
                            .cursor_pointer()
                            .icon(IconName::Trash)
                            .ghost()
                            .xsmall()
                            .tooltip("Delete…")
                            .on_click(cx.listener(|view, _, window, cx| view.delete(window, cx))),
                    ),
            )
            .child(
                h_flex()
                    .gap(dp(10.))
                    .text_size(dp(12.))
                    .text_color(t.text_muted)
                    .child(facts.join(" · "))
                    .child(div().flex_1())
                    .child(
                        Button::new("key-ttl")
                            .cursor_pointer()
                            .icon(IconName::Clock)
                            .label(ttl_label)
                            .ghost()
                            .xsmall()
                            .when(left.is_some(), |button| button.text_color(t.warning))
                            .dropdown_menu(move |menu, _, _| {
                                let choices: [(&str, Option<i64>); 4] = [
                                    ("Expire in 1 minute", Some(60)),
                                    ("Expire in 1 hour", Some(3600)),
                                    ("Expire in 1 day", Some(86400)),
                                    ("No expiry (persist)", None),
                                ];
                                choices.into_iter().fold(menu, |menu, (label, seconds)| {
                                    let view = view.clone();
                                    menu.item(PopupMenuItem::new(label).on_click(
                                        move |_, window, cx| {
                                            view.update(cx, |view, cx| {
                                                view.set_ttl(seconds, window, cx)
                                            });
                                        },
                                    ))
                                })
                            }),
                    ),
            )
            .into_any_element()
    }

    fn render_rows(&self, cx: &mut Context<Self>) -> AnyElement {
        let t = tokens(cx);
        let kind = self.kind();
        let read_only = kind == "stream";
        let (first_title, second_title) = match kind.as_str() {
            "hash" => ("FIELD", "VALUE"),
            "list" => ("#", "VALUE"),
            "zset" => ("MEMBER", "SCORE"),
            "stream" => ("ID", "FIELDS"),
            _ => ("MEMBER", ""),
        };
        let narrow_first = matches!(kind.as_str(), "list");
        let first_width = if narrow_first { dp(48.) } else { dp(180.) };
        let has_second = !second_title.is_empty();
        let header = |label: &'static str| {
            div()
                .text_size(dp(11.))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(t.text_muted)
                .child(label)
        };
        let rows: Vec<AnyElement> = self
            .rows
            .iter()
            .enumerate()
            .map(|(index, row)| {
                let state = self.row_state(row, cx);
                let tint = match state {
                    RowState::Unchanged => None,
                    RowState::Altered => Some(t.warning),
                    RowState::New => Some(t.success),
                    RowState::Removed => Some(t.danger),
                };
                let key = row.first.entity_id();
                let first: AnyElement = if read_only || (narrow_first && row.original.is_some()) {
                    div()
                        .text_size(dp(12.))
                        .text_color(t.text_muted)
                        .truncate()
                        .child(row.first.read(cx).value().to_string())
                        .into_any_element()
                } else if narrow_first {
                    div()
                        .text_size(dp(12.))
                        .text_color(t.success)
                        .child("+")
                        .into_any_element()
                } else {
                    Input::new(&row.first).small().into_any_element()
                };
                h_flex()
                    .id(("key-item", index))
                    .gap(dp(8.))
                    .px(dp(12.))
                    .py(dp(3.))
                    .border_l_2()
                    .border_color(tint.unwrap_or(transparent_black()))
                    .when_some(tint, |line, tint| line.bg(tint.opacity(0.07)))
                    .when(state == RowState::Removed, |line| line.opacity(0.5))
                    .child(div().w(first_width).flex_none().child(first))
                    .when(has_second, |line| {
                        line.child(div().flex_1().min_w_0().child(if read_only {
                            div()
                                .text_size(dp(12.))
                                .truncate()
                                .child(row.second.read(cx).value().to_string())
                                .into_any_element()
                        } else {
                            Input::new(&row.second).small().into_any_element()
                        }))
                    })
                    .when(!has_second, |line| line.child(div().flex_1()))
                    .when(!read_only, |line| {
                        line.child(
                            Button::new(("remove-item", index))
                                .cursor_pointer()
                                .icon(if row.removed {
                                    IconName::Undo
                                } else {
                                    IconName::Trash
                                })
                                .ghost()
                                .xsmall()
                                .on_click(cx.listener(move |view, _, _, cx| {
                                    let Some(position) = view
                                        .rows
                                        .iter()
                                        .position(|row| row.first.entity_id() == key)
                                    else {
                                        return;
                                    };
                                    if view.rows[position].original.is_none() {
                                        view.rows.remove(position);
                                    } else if let Some(row) = view.row_mut(key) {
                                        row.removed = !row.removed;
                                    }
                                    cx.notify();
                                })),
                        )
                    })
                    .into_any_element()
            })
            .collect();
        let truncated = self.detail.as_ref().is_some_and(|detail| detail.truncated);
        v_flex()
            .flex_1()
            .min_h_0()
            .child(
                h_flex()
                    .gap(dp(8.))
                    .px(dp(14.))
                    .py(dp(6.))
                    .child(div().w(first_width).child(header(first_title)))
                    .child(div().flex_1().child(header(second_title))),
            )
            .child(
                div()
                    .id("key-items")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(v_flex().children(rows))
                    .when(truncated, |list| {
                        list.child(
                            div()
                                .px(dp(14.))
                                .py(dp(6.))
                                .text_size(dp(12.))
                                .text_color(t.text_muted)
                                .child(format!(
                                    "Showing the first {KEY_ITEMS_CAP}; use the query editor for the rest."
                                )),
                        )
                    })
                    .when(!read_only, |list| {
                        list.child(
                            h_flex().px(dp(8.)).py(dp(6.)).child(
                                Button::new("add-item")
                                    .cursor_pointer()
                                    .icon(IconName::Plus)
                                    .label(match kind.as_str() {
                                        "hash" => "Field",
                                        "list" => "Item",
                                        _ => "Member",
                                    })
                                    .ghost()
                                    .small()
                                    .on_click(cx.listener(|view, _, window, cx| {
                                        view.push_row(None, window, cx);
                                        cx.notify();
                                    })),
                            ),
                        )
                    }),
            )
            .into_any_element()
    }

    fn render_bar(&self, count: usize, cx: &mut Context<Self>) -> AnyElement {
        let t = tokens(cx);
        h_flex()
            .flex_none()
            .gap(dp(8.))
            .px(dp(12.))
            .py(dp(8.))
            .border_t_1()
            .border_color(t.border)
            .bg(t.raised)
            .text_size(dp(12.5))
            .child(div().size(dp(8.)).rounded_full().bg(t.warning))
            .child(
                div()
                    .flex_1()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(if count == 1 {
                        "1 change".to_string()
                    } else {
                        format!("{count} changes")
                    }),
            )
            .child(
                Button::new("key-preview")
                    .cursor_pointer()
                    .label(if self.show_preview {
                        "Hide commands"
                    } else {
                        "Preview"
                    })
                    .ghost()
                    .small()
                    .on_click(cx.listener(|view, _, _, cx| {
                        view.show_preview = !view.show_preview;
                        cx.notify();
                    })),
            )
            .child(
                Button::new("key-discard")
                    .cursor_pointer()
                    .label("Discard")
                    .ghost()
                    .small()
                    .on_click(cx.listener(|view, _, window, cx| view.discard(window, cx))),
            )
            .child(
                Button::new("key-save")
                    .cursor_pointer()
                    .label("Save")
                    .primary()
                    .small()
                    .loading(self.saving)
                    .on_click(cx.listener(|view, _, window, cx| view.save(window, cx))),
            )
            .into_any_element()
    }
}

impl Render for RedisKeyView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = tokens(cx);
        let mono = cx.theme().mono_font_family.clone();
        let panel = v_flex().size_full().bg(t.surface);
        let Some(detail) = self.detail.clone() else {
            let message = if self.key.is_none() {
                "Select a key to see and edit its value."
            } else if self.loading {
                "Reading the key…"
            } else {
                "This key couldn't be read."
            };
            return panel
                .items_center()
                .justify_center()
                .gap(dp(8.))
                .text_size(dp(12.5))
                .text_color(t.text_muted)
                .child(Icon::new(IconName::KeyRound).text_color(t.text_faint))
                .child(message)
                .when_some(self.error.clone(), |panel, error| {
                    panel.child(div().px(dp(16.)).text_color(t.danger).child(error))
                });
        };
        if detail.kind == "none" {
            return panel
                .items_center()
                .justify_center()
                .text_size(dp(12.5))
                .text_color(t.text_muted)
                .child("This key no longer exists (it expired or was deleted).");
        }
        let count = self.change_count(cx);
        let body = if detail.kind == "string" {
            div()
                .flex_1()
                .min_h_0()
                .p(dp(10.))
                .child(Editor::new(&self.text).h_full())
                .into_any_element()
        } else {
            self.render_rows(cx)
        };
        let commands = if self.show_preview && count > 0 {
            Some(match self.edits(cx) {
                Ok(edits) => preview(&detail.key, &edits),
                Err(error) => format!("-- {error:#}"),
            })
        } else {
            None
        };
        panel
            .child(self.render_header(&detail, cx))
            .child(body)
            .when_some(self.error.clone(), |panel, error| {
                panel.child(
                    div()
                        .px(dp(14.))
                        .py(dp(8.))
                        .text_size(dp(12.))
                        .text_color(t.danger)
                        .bg(t.danger.opacity(0.08))
                        .child(error),
                )
            })
            .when_some(commands, |panel, commands| {
                panel.child(
                    div()
                        .id("key-commands")
                        .flex_none()
                        .max_h(dp(140.))
                        .overflow_y_scroll()
                        .mx(dp(12.))
                        .mb(dp(8.))
                        .p(dp(10.))
                        .rounded(dp(8.))
                        .border_1()
                        .border_color(t.border)
                        .bg(t.canvas)
                        .font_family(mono)
                        .text_size(dp(11.5))
                        .text_color(t.text_secondary)
                        .children(
                            commands
                                .lines()
                                .map(|line| div().child(line.to_string()))
                                .collect::<Vec<_>>(),
                        ),
                )
            })
            .when(count > 0, |panel| panel.child(self.render_bar(count, cx)))
    }
}
