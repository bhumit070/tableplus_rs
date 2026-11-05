//! SQL / command editor with a results pane. ⌘↵ runs the selection, or the
//! whole editor when nothing is selected. Each statement gets a result tab.

use std::rc::Rc;

use gpui::{
    App, ClipboardItem, Context, Entity, FocusHandle, Focusable, IntoElement, ParentElement,
    Render, SharedString, Styled, Window, div, prelude::*, px, rems,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::{Editor, EditorState};
use gpui_component::menu::PopupMenuItem;
use gpui_component::notification::Notification;
use gpui_component::tab::{Tab, TabBar};
use gpui_component::table::TableState;
use gpui_component::{
    ActiveTheme as _, Sizable as _, WindowExt as _, h_flex, resizable_panel, v_flex, v_resizable,
};
use gpui_kit_assets::IconName;

use super::grid::{Grid, data_table};
use super::{RunQuery, Shared, is_read_only};
use crate::db::{Driver, EnvTag, QueryResult};
use crate::runtime;

pub struct QueryTab {
    shared: Rc<Shared>,
    pub title: String,
    editor: Entity<EditorState>,
    results: Vec<QueryResult>,
    active: usize,
    grid: Entity<TableState<Grid>>,
    running: bool,
    error: Option<String>,
    elapsed: u128,
    focus: FocusHandle,
}

fn placeholder(d: Driver) -> &'static str {
    match d {
        Driver::Redis => "GET mykey\nHGETALL user:1\nSCAN 0 MATCH user:* COUNT 100",
        Driver::Mongo => "db.users.find({ age: { $gt: 30 } }).sort({ name: 1 }).limit(50)",
        _ => "SELECT * FROM users WHERE id = 1;",
    }
}

impl QueryTab {
    pub fn new(
        shared: Rc<Shared>,
        n: usize,
        initial: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let lang = shared.cfg.driver.editor_language();
        let editor = cx.new(|cx| {
            EditorState::new(window, cx)
                .language(lang)
                .line_number(true)
                .soft_wrap(false)
                .placeholder(placeholder(shared.cfg.driver))
                .default_value(initial.to_string())
        });
        let provider = super::completion::Provider {
            schema: shared.schema.clone(),
            driver: shared.cfg.driver,
        };
        editor.update(cx, |e, _| {
            e.lsp_mut().completion_provider = Some(Rc::new(provider))
        });
        let weak = cx.entity().downgrade();
        let mut grid = Grid::default();
        grid.on_menu = Some(Rc::new(move |row, menu, _, _| {
            let w = weak.clone();
            let w2 = weak.clone();
            menu.item(
                PopupMenuItem::new("Copy Row as JSON").on_click(move |_, window, cx| {
                    w.update(cx, |t, cx| {
                        let json = t.grid.read(cx).delegate().row_json(row);
                        cx.write_to_clipboard(ClipboardItem::new_string(json));
                        window.push_notification("Row copied as JSON", cx);
                    })
                    .ok();
                }),
            )
            .item(
                PopupMenuItem::new("Copy Result as CSV").on_click(move |_, window, cx| {
                    w2.update(cx, |t, cx| {
                        let csv = t.grid.read(cx).delegate().to_csv();
                        cx.write_to_clipboard(ClipboardItem::new_string(csv));
                        window.push_notification("Result copied as CSV", cx);
                    })
                    .ok();
                }),
            )
        }));
        let grid = cx.new(|cx| {
            TableState::new(grid, window, cx)
                .cell_selectable(true)
                .row_selectable(true)
                .col_resizable(true)
                .sortable(true)
        });
        Self {
            shared,
            title: format!("Query {n}"),
            editor,
            results: Vec::new(),
            active: 0,
            grid,
            running: false,
            error: None,
            elapsed: 0,
            focus: cx.focus_handle(),
        }
    }

    pub fn focus_editor(this: &Entity<Self>, window: &mut Window, cx: &mut App) {
        let editor = this.read(cx).editor.clone();
        editor.update(cx, |e, cx| e.focus(window, cx));
    }

    pub fn run(&mut self, _: &RunQuery, window: &mut Window, cx: &mut Context<Self>) {
        if self.running {
            return;
        }
        let e = self.editor.read(cx);
        let sel = e.selected_value();
        let text = if sel.trim().is_empty() {
            e.value().to_string()
        } else {
            sel.to_string()
        };
        if text.trim().is_empty() {
            return;
        }
        if self.shared.cfg.tag == EnvTag::Production && !is_read_only(&text) {
            let weak = cx.entity().downgrade();
            let shown: SharedString = text.chars().take(600).collect::<String>().into();
            window.open_alert_dialog(cx, move |d, _, _| {
                let weak = weak.clone();
                let text = text.clone();
                d.title("Run on PRODUCTION?")
                    .description(shown.clone())
                    .on_ok(move |_, window, cx| {
                        weak.update(cx, |t, cx| t.execute(text.clone(), window, cx))
                            .ok();
                        true
                    })
            });
        } else {
            self.execute(text, window, cx);
        }
    }

    fn execute(&mut self, text: String, window: &mut Window, cx: &mut Context<Self>) {
        self.running = true;
        self.error = None;
        cx.notify();
        let conn = self.shared.conn.clone();
        let start = std::time::Instant::now();
        let task = runtime::spawn(async move { conn.read().await.execute(&text).await });
        cx.spawn_in(window, async move |this, cx| {
            let res = task.await;
            this.update_in(cx, |this, window, cx| {
                this.running = false;
                this.elapsed = start.elapsed().as_millis();
                match res {
                    Ok(results) => {
                        // Show the last statement that returned rows, else the last one.
                        this.active = results
                            .iter()
                            .rposition(|r| !r.columns.is_empty())
                            .unwrap_or(results.len().saturating_sub(1));
                        this.results = results;
                        this.show_active(cx);
                    }
                    Err(e) => {
                        this.error = Some(format!("{e:#}"));
                        window.push_notification(Notification::error("Query failed"), cx);
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn show_active(&mut self, cx: &mut Context<Self>) {
        let res = self.results.get(self.active).cloned().unwrap_or_default();
        self.grid.update(cx, |t, cx| {
            let g = t.delegate_mut();
            g.sort = None;
            g.set_result(&res);
            t.refresh(cx);
            cx.notify();
        });
    }

    fn render_results(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let current = self.results.get(self.active);
        let summary = match current {
            _ if self.running => "Running…".to_string(),
            None => "⌘↵ to run".to_string(),
            Some(r) if r.columns.is_empty() => {
                format!(
                    "{} row(s) affected · {} ms",
                    r.affected.unwrap_or(0),
                    r.elapsed_ms
                )
            }
            Some(r) => format!("{} row(s) · {} ms", r.rows.len(), r.elapsed_ms),
        };
        let body = if let Some(e) = &self.error {
            div()
                .size_full()
                .p_3()
                .text_sm()
                .text_color(theme.danger)
                .font_family(theme.mono_font_family.clone())
                .child(e.clone())
                .into_any_element()
        } else if current.is_some_and(|r| r.columns.is_empty()) {
            div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .text_color(theme.muted_foreground)
                .child(summary.clone())
                .into_any_element()
        } else {
            data_table(&self.grid, cx).into_any_element()
        };
        v_flex()
            .size_full()
            .when(self.results.len() > 1, |d| {
                d.child(
                    TabBar::new("results")
                        .underline()
                        .small()
                        .selected_index(self.active)
                        .children(self.results.iter().enumerate().map(|(i, r)| {
                            let first = r
                                .statement
                                .lines()
                                .next()
                                .unwrap_or("")
                                .chars()
                                .take(28)
                                .collect::<String>();
                            Tab::new().label(format!("{}: {first}", i + 1))
                        }))
                        .on_click(cx.listener(|this, ix: &usize, _, cx| {
                            this.active = *ix;
                            this.show_active(cx);
                            cx.notify();
                        })),
                )
            })
            .child(div().flex_1().min_h_0().child(body))
            .child(
                h_flex()
                    .h(rems(1.875))
                    .px_2()
                    .border_t_1()
                    .border_color(theme.border)
                    .bg(theme.title_bar)
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(summary),
            )
    }
}

impl Focusable for QueryTab {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for QueryTab {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let toolbar = h_flex()
            .h(rems(2.25))
            .px_2()
            .gap_1()
            .border_b_1()
            .border_color(theme.border)
            .child(
                Button::new("run")
                    .icon(IconName::Play)
                    .label("Run")
                    .primary()
                    .small()
                    .loading(self.running)
                    .tooltip("Run selection or all (⌘↵)")
                    .on_click(cx.listener(|this, _, window, cx| this.run(&RunQuery, window, cx))),
            )
            .child(div().text_xs().text_color(theme.muted_foreground).child(
                match self.shared.cfg.driver {
                    Driver::Redis => "One command per line",
                    Driver::Mongo => {
                        "mongosh syntax: db.coll.find({...}), aggregate([...]), or a {command}"
                    }
                    _ => "Separate statements with ;",
                },
            ));
        v_flex()
            .size_full()
            .key_context("QueryTab")
            .track_focus(&self.focus)
            .child(toolbar)
            .child(
                v_resizable("query-split")
                    .child(
                        resizable_panel().child(Editor::new(&self.editor).bordered(false).h_full()),
                    )
                    .child(
                        resizable_panel()
                            .size(px(320.))
                            .child(self.render_results(cx)),
                    ),
            )
    }
}
