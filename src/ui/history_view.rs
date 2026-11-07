//! Query history (⌘Y) and saved queries (⌘⇧O) in one dialog (design.md,
//! screen 15): two tabs, a search, an all / ok / failed filter for history,
//! the list on the left and the chosen query's full SQL on the right with
//! Open in new tab, Insert at cursor and Save as… (or Delete for a saved
//! one). ↑ / ↓ move, ↵ opens.

use std::rc::Rc;

use gpui::{
    AnyElement, App, AppContext as _, ClickEvent, Context, Entity, FontWeight, IntoElement,
    KeyDownEvent, ParentElement as _, Render, SharedString, Styled as _, Subscription, WeakEntity,
    Window, div, prelude::*,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::notification::Notification;
use gpui_component::spinner::Spinner;
use gpui_component::{ActiveTheme as _, Icon, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit_assets::IconName;

use super::dialog;
use super::session::Session;
use super::theme::{caps, dp, kbd, tokens};
use crate::history::{self, Entry};
use crate::runtime;
use crate::saved::{self, SavedQuery};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    History,
    Saved,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Status {
    All,
    Ok,
    Failed,
}

/// A row of the list: an index into `entries` or `saved`.
#[derive(Clone, Copy)]
enum Item {
    Run(usize),
    Saved(usize),
}

pub struct HistoryView {
    session: WeakEntity<Session>,
    conn_id: String,
    tab: Tab,
    status: Status,
    search: Entity<InputState>,
    /// `None` while loading.
    entries: Option<Vec<Entry>>,
    load_error: Option<String>,
    saved: Vec<SavedQuery>,
    selected: usize,
    _subs: Vec<Subscription>,
}

impl HistoryView {
    pub fn open(
        session: WeakEntity<Session>,
        conn_id: String,
        tab: Tab,
        window: &mut Window,
        cx: &mut App,
    ) -> Entity<Self> {
        let view = cx.new(|cx| Self::new(session, conn_id, tab, window, cx));
        let shown = view.clone();
        let w = dialog::width(920., cx);
        let first = std::cell::Cell::new(true);
        window.open_dialog(cx, move |d, window, cx| {
            if first.replace(false) {
                let search = view.read(cx).search.clone();
                search.update(cx, |s, cx| s.focus(window, cx));
            }
            let v = view.clone();
            dialog::shell(d, w, cx)
                .child(view.clone())
                // ↵ in the search box: open the highlighted query.
                .on_ok(move |_, window, cx| {
                    v.update(cx, |v, cx| v.open_selected(window, cx));
                    false
                })
        });
        shown
    }

    /// For screenshots: whether the history has been read.
    #[cfg(test)]
    pub fn loaded(&self) -> bool {
        self.entries.is_some()
    }

    fn new(
        session: WeakEntity<Session>,
        conn_id: String,
        tab: Tab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search SQL…"));
        let subs = vec![
            cx.subscribe_in(&search, window, |this, _, ev: &InputEvent, _, cx| {
                if let InputEvent::Change = ev {
                    this.selected = 0;
                    cx.notify();
                }
            }),
        ];
        let (saved, load_error) = match saved::for_connection(&conn_id) {
            Ok(s) => (s, None),
            Err(e) => (vec![], Some(format!("{e:#}"))),
        };
        let id = conn_id.clone();
        let task = runtime::spawn(async move { history::recent(&id, 1000).await });
        cx.spawn(async move |this, cx| {
            let res = task.await;
            this.update(cx, |this, cx| {
                match res {
                    Ok(e) => this.entries = Some(e),
                    Err(e) => {
                        this.entries = Some(vec![]);
                        this.load_error = Some(format!("Couldn't read query history: {e:#}"));
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        Self {
            session,
            conn_id,
            tab,
            status: Status::All,
            search,
            entries: None,
            load_error,
            saved,
            selected: 0,
            _subs: subs,
        }
    }

    /// What the list shows, after the search and the filter.
    fn items(&self, cx: &App) -> Vec<Item> {
        let q = self.search.read(cx).value().trim().to_lowercase();
        let hit = |s: &str| q.is_empty() || s.to_lowercase().contains(&q);
        match self.tab {
            Tab::History => self
                .entries
                .iter()
                .flatten()
                .enumerate()
                .filter(|(_, e)| match self.status {
                    Status::All => true,
                    Status::Ok => e.error.is_none(),
                    Status::Failed => e.error.is_some(),
                })
                .filter(|(_, e)| hit(&e.sql))
                .map(|(i, _)| Item::Run(i))
                .collect(),
            Tab::Saved => {
                // This connection's first, then those shown everywhere.
                let mut idx: Vec<usize> = (0..self.saved.len())
                    .filter(|&i| hit(&self.saved[i].name) || hit(&self.saved[i].sql))
                    .collect();
                idx.sort_by_key(|&i| self.saved[i].conn_id.is_none());
                idx.into_iter().map(Item::Saved).collect()
            }
        }
    }

    fn sql(&self, item: Item) -> String {
        match item {
            Item::Run(i) => self.entries.as_ref().map(|e| e[i].sql.clone()),
            Item::Saved(i) => Some(self.saved[i].sql.clone()),
        }
        .unwrap_or_default()
    }

    fn current(&self, cx: &App) -> Option<Item> {
        let items = self.items(cx);
        items
            .get(self.selected.min(items.len().saturating_sub(1)))
            .copied()
    }

    fn set_tab(&mut self, tab: Tab, cx: &mut Context<Self>) {
        self.tab = tab;
        self.selected = 0;
        cx.notify();
    }

    fn open_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(item) = self.current(cx) else {
            return;
        };
        let sql = self.sql(item);
        let saved = match item {
            Item::Saved(i) => Some(self.saved[i].clone()),
            Item::Run(_) => None,
        };
        window.close_dialog(cx);
        self.session
            .update(cx, |s, cx| {
                s.open_query(sql, window, cx);
                if let (Some(q), Some(tab)) = (saved, s.active_query()) {
                    tab.update(cx, |t, _| t.set_saved(q));
                }
            })
            .ok();
    }

    /// Into the open query tab at its cursor, or a new tab when the current
    /// tab isn't a query.
    fn insert_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(item) = self.current(cx) else {
            return;
        };
        let sql = self.sql(item);
        window.close_dialog(cx);
        self.session
            .update(cx, |s, cx| match s.active_query() {
                Some(tab) => tab.update(cx, |t, cx| t.insert_text(&sql, window, cx)),
                None => s.open_query(sql, window, cx),
            })
            .ok();
    }

    /// Opens the query in a new tab and asks for a name there.
    fn save_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(item) = self.current(cx) else {
            return;
        };
        let sql = self.sql(item);
        window.close_dialog(cx);
        let session = self.session.clone();
        // After this dialog has gone, so the name dialog isn't closed with it.
        window.defer(cx, move |window, cx| {
            session
                .update(cx, |s, cx| {
                    s.open_query(sql, window, cx);
                    if let Some(tab) = s.active_query() {
                        tab.update(cx, |t, cx| t.save(window, cx));
                    }
                })
                .ok();
        });
    }

    fn delete_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(Item::Saved(i)) = self.current(cx) else {
            return;
        };
        let q = self.saved[i].clone();
        match saved::delete(&q.id) {
            Ok(()) => {
                self.saved.remove(i);
                window.push_notification(format!("Deleted “{}”", q.name), cx);
            }
            Err(e) => window.push_notification(Notification::error(format!("{e:#}")), cx),
        }
        cx.notify();
    }

    fn clear_history(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let id = self.conn_id.clone();
        let task = runtime::spawn(async move { history::clear(&id).await });
        cx.spawn_in(window, async move |this, cx| {
            let res = task.await;
            this.update_in(cx, |this, window, cx| {
                match res {
                    Ok(()) => {
                        this.entries = Some(vec![]);
                        window.push_notification("Query history cleared", cx);
                    }
                    Err(e) => window.push_notification(Notification::error(format!("{e:#}")), cx),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn on_key(&mut self, ev: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let n = self.items(cx).len();
        if n == 0 {
            return;
        }
        match ev.keystroke.key.as_str() {
            "down" => self.selected = (self.selected + 1).min(n - 1),
            "up" => self.selected = self.selected.saturating_sub(1),
            _ => return,
        }
        cx.stop_propagation();
        cx.notify();
    }

    fn render_row(&self, row: usize, item: Item, cx: &mut Context<Self>) -> AnyElement {
        let t = tokens(cx);
        let active = row == self.selected;
        let mono = cx.theme().mono_font_family.clone();
        let now = history::now();
        let (dot, top, sql, bottom, bottom_color) = match item {
            Item::Run(i) => {
                let e = &self.entries.as_ref().expect("loaded")[i];
                let (dot, bottom, color) = match (&e.error, e.rows) {
                    (Some(err), _) => (
                        t.danger,
                        err.lines().next().unwrap_or_default().to_string(),
                        t.danger,
                    ),
                    (None, rows) => (
                        t.success,
                        format!(
                            "{}{} ms",
                            rows.map(|n| format!("{n} rows · ")).unwrap_or_default(),
                            e.elapsed_ms
                        ),
                        t.text_muted,
                    ),
                };
                let top = if e.database.is_empty() {
                    history::ago(e.ran_at, now)
                } else {
                    format!("{} · {}", history::ago(e.ran_at, now), e.database)
                };
                (Some(dot), top, e.sql.clone(), bottom, color)
            }
            Item::Saved(i) => {
                let q = &self.saved[i];
                (
                    None,
                    q.name.clone(),
                    q.sql.clone(),
                    String::new(),
                    t.text_muted,
                )
            }
        };
        // Two lines of SQL, whitespace squeezed.
        let lines: Vec<String> = sql
            .lines()
            .map(|l| l.split_whitespace().collect::<Vec<_>>().join(" "))
            .filter(|l| !l.is_empty())
            .take(2)
            .collect();
        v_flex()
            .id(("history-row", row))
            .debug_selector(move || format!("history-row-{row}"))
            .px(dp(10.))
            .py(dp(8.))
            .gap(dp(3.))
            .rounded(dp(7.))
            .cursor_pointer()
            .when(active, |d| d.bg(t.accent.opacity(0.14)))
            .when(!active, |d| d.hover(|s| s.bg(t.hover)))
            .child(
                h_flex()
                    .gap(dp(6.))
                    .text_size(dp(11.5))
                    .text_color(t.text_muted)
                    .when_some(dot, |d, dot| {
                        d.child(div().flex_none().size(dp(6.)).rounded_full().bg(dot))
                    })
                    .when(matches!(item, Item::Saved(_)), |d| {
                        d.text_color(t.text)
                            .font_weight(FontWeight::MEDIUM)
                            .text_size(dp(12.5))
                    })
                    .child(div().truncate().child(top)),
            )
            .child(
                v_flex()
                    .font_family(mono)
                    .text_size(dp(11.5))
                    .text_color(t.text_secondary)
                    .children(lines.into_iter().map(|l| div().truncate().child(l))),
            )
            .when(!bottom.is_empty(), |d| {
                d.child(
                    div()
                        .truncate()
                        .text_size(dp(11.))
                        .text_color(bottom_color)
                        .child(bottom),
                )
            })
            .on_click(cx.listener(move |this, ev: &ClickEvent, window, cx| {
                this.selected = row;
                if ev.click_count() == 2 {
                    this.open_selected(window, cx);
                }
                cx.notify();
            }))
            .into_any_element()
    }
}

impl Render for HistoryView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = tokens(cx);
        let items = self.items(cx);
        if self.selected >= items.len() {
            self.selected = items.len().saturating_sub(1);
        }
        let me = cx.entity();
        let tabs = dialog::segmented(
            "history-tab",
            &["History", "Saved"],
            (self.tab == Tab::Saved) as usize,
            Rc::new({
                let me = me.clone();
                move |i, _, cx| {
                    me.update(cx, |v, cx| {
                        v.set_tab(if i == 0 { Tab::History } else { Tab::Saved }, cx)
                    })
                }
            }),
            cx,
        )
        .w(dp(200.));
        let filter = dialog::segmented(
            "history-status",
            &["All", "OK", "Failed"],
            match self.status {
                Status::All => 0,
                Status::Ok => 1,
                Status::Failed => 2,
            },
            Rc::new(move |i, _, cx| {
                me.update(cx, |v, cx| {
                    v.status = [Status::All, Status::Ok, Status::Failed][i];
                    v.selected = 0;
                    cx.notify();
                })
            }),
            cx,
        )
        .w(dp(190.));

        let loading = self.tab == Tab::History && self.entries.is_none();
        let rows: Vec<AnyElement> = {
            let mut out = vec![];
            let mut group: Option<bool> = None;
            for (row, item) in items.iter().enumerate() {
                // Saved queries: a heading per scope.
                if let Item::Saved(i) = item {
                    let everywhere = self.saved[*i].conn_id.is_none();
                    if group != Some(everywhere) {
                        group = Some(everywhere);
                        out.push(
                            div()
                                .px(dp(10.))
                                .pt(dp(if row == 0 { 2. } else { 10. }))
                                .pb(dp(4.))
                                .child(caps(
                                    if everywhere {
                                        "All connections"
                                    } else {
                                        "This connection"
                                    },
                                    cx,
                                ))
                                .into_any_element(),
                        );
                    }
                }
                out.push(self.render_row(row, *item, cx));
            }
            out
        };
        let empty_text = match self.tab {
            Tab::History if self.entries.as_ref().is_some_and(|e| e.is_empty()) => {
                "No queries run on this connection yet."
            }
            Tab::Saved if self.saved.is_empty() => "No saved queries yet: save one with ⌘⇧S.",
            _ => "Nothing matches.",
        };
        let list = div()
            .id("history-list")
            .w(dp(380.))
            .flex_none()
            .h_full()
            .overflow_y_scroll()
            .pr(dp(6.))
            .map(|d| {
                if loading {
                    d.child(
                        h_flex()
                            .gap(dp(8.))
                            .p(dp(12.))
                            .text_size(dp(12.5))
                            .text_color(t.text_muted)
                            .child(Spinner::new().small())
                            .child("Loading history…"),
                    )
                } else if rows.is_empty() {
                    d.child(
                        div()
                            .p(dp(12.))
                            .text_size(dp(12.5))
                            .text_color(t.text_muted)
                            .child(empty_text),
                    )
                } else {
                    d.child(v_flex().gap(dp(2.)).children(rows))
                }
            });

        let current = self.current(cx);
        let preview = v_flex()
            .flex_1()
            .min_w_0()
            .h_full()
            .gap(dp(10.))
            .map(|d| match current {
                None => d.child(
                    div()
                        .flex_1()
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_size(dp(12.5))
                        .text_color(t.text_faint)
                        .child("Choose a query to see it in full."),
                ),
                Some(item) => {
                    let error = match item {
                        Item::Run(i) => self.entries.as_ref().and_then(|e| e[i].error.clone()),
                        Item::Saved(_) => None,
                    };
                    d.child(
                        div()
                            .id("history-sql")
                            .flex_1()
                            .min_h_0()
                            .overflow_y_scroll()
                            .p(dp(12.))
                            .rounded(dp(8.))
                            .border_1()
                            .border_color(t.border)
                            .bg(t.canvas)
                            .font_family(cx.theme().mono_font_family.clone())
                            .text_size(dp(12.))
                            .children(
                                self.sql(item)
                                    .lines()
                                    .map(|l| div().child(l.to_string()))
                                    .collect::<Vec<_>>(),
                            ),
                    )
                    .when_some(error, |d, e| {
                        d.child(
                            div()
                                .p(dp(10.))
                                .rounded(dp(8.))
                                .bg(t.danger.opacity(0.08))
                                .text_size(dp(11.5))
                                .text_color(t.danger)
                                .child(e),
                        )
                    })
                    .child(
                        h_flex()
                            .gap(dp(8.))
                            .child(
                                Button::new("history-open")
                                    .cursor_pointer()
                                    .icon(IconName::SquarePlus)
                                    .label("Open in new tab")
                                    .primary()
                                    .small()
                                    .on_click(
                                        cx.listener(|v, _, window, cx| v.open_selected(window, cx)),
                                    ),
                            )
                            .child(
                                div().debug_selector(|| "history-insert".into()).child(
                                    Button::new("history-insert")
                                        .cursor_pointer()
                                        .icon(IconName::TextCursorInput)
                                        .label("Insert at cursor")
                                        .outline()
                                        .small()
                                        .on_click(cx.listener(|v, _, window, cx| {
                                            v.insert_selected(window, cx)
                                        })),
                                ),
                            )
                            .child(match item {
                                Item::Run(_) => Button::new("history-save")
                                    .cursor_pointer()
                                    .icon(IconName::Star)
                                    .label("Save as…")
                                    .ghost()
                                    .small()
                                    .on_click(
                                        cx.listener(|v, _, window, cx| v.save_selected(window, cx)),
                                    ),
                                Item::Saved(_) => Button::new("history-delete")
                                    .cursor_pointer()
                                    .icon(IconName::Trash)
                                    .label("Delete")
                                    .ghost()
                                    .small()
                                    .on_click(cx.listener(|v, _, window, cx| {
                                        v.delete_selected(window, cx)
                                    })),
                            }),
                    )
                }
            });

        let hint: SharedString = "↑ ↓ move · ↵ open in a new tab · Esc closes".into();
        v_flex()
            .gap(dp(14.))
            .capture_key_down(cx.listener(Self::on_key))
            .child(
                h_flex()
                    .gap(dp(12.))
                    .child(dialog::title(
                        match self.tab {
                            Tab::History => "Query history",
                            Tab::Saved => "Saved queries",
                        },
                        None,
                        cx,
                    ))
                    .child(div().flex_1())
                    .child(tabs),
            )
            .child(
                h_flex()
                    .gap(dp(10.))
                    .child(
                        div().flex_1().child(
                            Input::new(&self.search)
                                .prefix(Icon::new(IconName::Search).small().text_color(t.text_muted)),
                        ),
                    )
                    .when(self.tab == Tab::History, |d| d.child(filter)),
            )
            .when_some(self.load_error.clone(), |d, e| {
                d.child(
                    div()
                        .text_size(dp(12.))
                        .text_color(t.danger)
                        .child(e),
                )
            })
            .child(
                h_flex()
                    .h(dp(440.))
                    .gap(dp(14.))
                    .items_start()
                    .child(list)
                    .child(preview),
            )
            .child(dialog::footer(
                hint,
                if self.tab == Tab::History
                    && self.entries.as_ref().is_some_and(|e| !e.is_empty())
                {
                    vec![
                        Button::new("history-clear")
                            .cursor_pointer()
                            .label("Clear history…")
                            .ghost()
                            .small()
                            .on_click(cx.listener(|_, _, window, cx| {
                                let me = cx.entity().downgrade();
                                window.open_alert_dialog(cx, move |d, _, _| {
                                    let me = me.clone();
                                    d.title("Clear query history?")
                                        .description(
                                            "Deletes every query run on this connection from history.",
                                        )
                                        .show_cancel(true)
                                        .on_ok(move |_, window, cx| {
                                            me.update(cx, |v, cx| v.clear_history(window, cx))
                                                .ok();
                                            true
                                        })
                                });
                            }))
                            .into_any_element(),
                        kbd("⌘Y", cx).into_any_element(),
                    ]
                } else {
                    vec![kbd(
                        match self.tab {
                            Tab::History => "⌘Y",
                            Tab::Saved => "⌘⇧O",
                        },
                        cx,
                    )
                    .into_any_element()]
                },
                cx,
            ))
    }
}
