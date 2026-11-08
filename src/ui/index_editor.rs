//! Structure › Indexes (design.md screen 08): the table's indexes as a
//! list with a right-click menu (Copy definition, Drop index…) and the
//! small `+ Index` dialog (name, columns, unique, the statement, live).

use std::cell::Cell;
use std::rc::Rc;

use gpui::{
    AnyElement, App, AppContext as _, ClipboardItem, Context, Entity, FontWeight, IntoElement,
    ParentElement, Render, Styled, Window, div, prelude::*,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::checkbox::Checkbox;
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::menu::{ContextMenuExt as _, PopupMenuItem};
use gpui_component::notification::Notification;
use gpui_component::{ActiveTheme as _, Icon, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit_assets::IconName;

use super::confirm::ConfirmWrite;
use super::table_editor::OnDone;
use super::theme::{dp, tokens};
use super::{Shared, dialog, guarded};
use crate::db::ddl::{IndexEntry, create_index, drop_index};
use crate::db::safety::gate_write;
use crate::db::{Cancel, DbObject};
use crate::runtime;
use crate::ui::DisabledWhen as _;

/// Runs `statement`, then tells the tab to read the structure again.
pub(super) fn run_statement(
    shared: Rc<Shared>,
    statement: String,
    success: &'static str,
    on_done: OnDone,
    window: &mut Window,
    cx: &mut App,
) {
    let conn = shared.conn.clone();
    let task = runtime::spawn(async move {
        conn.read()
            .await
            .execute_cancellable(&statement, &Cancel::default())
            .await
    });
    window
        .spawn(cx, async move |cx| {
            let result = task.await;
            cx.update(|window, cx| match result {
                Ok(_) => {
                    window.push_notification(Notification::success(success), cx);
                    on_done(window, cx);
                }
                Err(error) => {
                    window.push_notification(Notification::error(format!("{error:#}")), cx)
                }
            })
            .ok();
        })
        .detach();
}

/// The index list, with a right-click menu per index.
pub fn render_list(
    shared: &Rc<Shared>,
    object: &DbObject,
    entries: &[IndexEntry],
    on_add: Rc<dyn Fn(&mut Window, &mut App)>,
    on_done: OnDone,
    cx: &App,
) -> AnyElement {
    let t = tokens(cx);
    let mono = cx.theme().mono_font_family.clone();
    v_flex()
        .size_full()
        .child(
            h_flex().px(dp(8.)).py(dp(6.)).child(
                div().debug_selector(|| "add-index".into()).child(
                    Button::new("add-index")
                        .cursor_pointer()
                        .icon(IconName::Plus)
                        .label("Index")
                        .ghost()
                        .small()
                        .on_click(move |_, window, cx| on_add(window, cx)),
                ),
            ),
        )
        .when(entries.is_empty(), |list| {
            list.child(
                div()
                    .px(dp(14.))
                    .text_size(dp(12.5))
                    .text_color(t.text_muted)
                    .child("No indexes yet."),
            )
        })
        .child(
            div()
                .id("index-list")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .children(entries.iter().enumerate().map(|(index, entry)| {
                    let (shared, object, entry_for_menu, on_done) = (
                        shared.clone(),
                        object.clone(),
                        entry.clone(),
                        on_done.clone(),
                    );
                    h_flex()
                        .id(("index-row", index))
                        .gap(dp(12.))
                        .px(dp(14.))
                        .py(dp(8.))
                        .border_b_1()
                        .border_color(t.border)
                        .hover(|row| row.bg(t.hover))
                        .child(
                            Icon::new(IconName::ListTree)
                                .small()
                                .text_color(t.text_muted),
                        )
                        .child(
                            div()
                                .w(dp(240.))
                                .flex_none()
                                .truncate()
                                .font_weight(FontWeight::MEDIUM)
                                .text_size(dp(13.))
                                .child(entry.name.clone()),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .font_family(mono.clone())
                                .text_size(dp(12.))
                                .text_color(t.text_secondary)
                                .child(entry.definition.clone()),
                        )
                        .context_menu(move |menu, _, _| {
                            let definition = entry_for_menu.definition.clone();
                            let name = entry_for_menu.name.clone();
                            let (shared, object, on_done) =
                                (shared.clone(), object.clone(), on_done.clone());
                            menu.item(PopupMenuItem::new("Copy Definition").on_click(
                                move |_, _, cx| {
                                    cx.write_to_clipboard(ClipboardItem::new_string(
                                        definition.clone(),
                                    ))
                                },
                            ))
                            .separator()
                            .item(
                                PopupMenuItem::new("Drop Index…").on_click(move |_, window, cx| {
                                    confirm_drop(
                                        shared.clone(),
                                        &object,
                                        &name,
                                        on_done.clone(),
                                        window,
                                        cx,
                                    )
                                }),
                            )
                        })
                })),
        )
        .into_any_element()
}

/// Dropping always asks first, whatever the safe mode.
pub fn confirm_drop(
    shared: Rc<Shared>,
    object: &DbObject,
    index_name: &str,
    on_done: OnDone,
    window: &mut Window,
    cx: &mut App,
) {
    let statement = drop_index(
        shared.cfg.driver,
        object.schema.as_deref(),
        &object.name,
        index_name,
    );
    let cfg = shared.cfg.clone();
    let to_run = statement.clone();
    ConfirmWrite::open(
        &cfg,
        "Drop index",
        statement,
        window,
        cx,
        Rc::new(move |window, cx| {
            run_statement(
                shared.clone(),
                to_run.clone(),
                "Index dropped",
                on_done.clone(),
                window,
                cx,
            )
        }),
    );
}

/// The `+ Index` dialog.
pub struct IndexDialog {
    shared: Rc<Shared>,
    object: DbObject,
    columns: Vec<String>,
    chosen: Vec<String>,
    unique: bool,
    index_name: Entity<InputState>,
    /// The name follows the chosen columns until it's typed over.
    name_typed: bool,
    on_done: OnDone,
}

impl IndexDialog {
    pub fn open(
        shared: Rc<Shared>,
        object: DbObject,
        columns: Vec<String>,
        on_done: OnDone,
        window: &mut Window,
        cx: &mut App,
    ) -> Entity<IndexDialog> {
        let view = cx.new(|cx| {
            let index_name = cx.new(|cx| InputState::new(window, cx).placeholder("index_name"));
            cx.subscribe(&index_name, |dialog: &mut IndexDialog, _, event, cx| {
                if let InputEvent::Change = event {
                    dialog.name_typed = true;
                    cx.notify();
                }
            })
            .detach();
            IndexDialog {
                shared,
                object,
                columns,
                chosen: vec![],
                unique: false,
                index_name,
                name_typed: false,
                on_done,
            }
        });
        let first = Cell::new(true);
        let result = view.clone();
        let width = dialog::width(520., cx);
        window.open_dialog(cx, move |dialog_box, window, cx| {
            if first.replace(false) {
                let name = view.read(cx).index_name.clone();
                name.update(cx, |input, cx| input.focus(window, cx));
            }
            dialog::shell(dialog_box, width, cx).child(view.clone())
        });
        result
    }

    /// `orders_status_created_at_idx`, for the chosen columns.
    fn suggested_name(&self) -> String {
        let mut parts = vec![self.object.name.clone()];
        parts.extend(self.chosen.iter().cloned());
        parts.push(if self.unique { "key" } else { "idx" }.into());
        parts.join("_")
    }

    fn update_name(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.name_typed {
            return;
        }
        let name = if self.chosen.is_empty() {
            String::new()
        } else {
            self.suggested_name()
        };
        self.index_name
            .update(cx, |input, cx| input.set_value(name, window, cx));
        // Setting it isn't the user typing.
        self.name_typed = false;
    }

    pub fn toggle_column(&mut self, column: &str, window: &mut Window, cx: &mut Context<Self>) {
        match self.chosen.iter().position(|chosen| chosen == column) {
            Some(index) => {
                self.chosen.remove(index);
            }
            None => self.chosen.push(column.to_string()),
        }
        self.update_name(window, cx);
        cx.notify();
    }

    fn statement(&self, cx: &App) -> anyhow::Result<String> {
        create_index(
            self.shared.cfg.driver,
            self.object.schema.as_deref(),
            &self.object.name,
            self.index_name.read(cx).value().as_ref(),
            &self.chosen,
            self.unique,
        )
    }

    pub fn create(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Ok(statement) = self.statement(cx) else {
            return;
        };
        let gate = gate_write(&self.shared.cfg);
        let shared = self.shared.clone();
        let on_done = self.on_done.clone();
        let to_run = statement.clone();
        let run = move |window: &mut Window, cx: &mut App| {
            window.close_dialog(cx);
            run_statement(
                shared.clone(),
                to_run.clone(),
                "Index created",
                on_done.clone(),
                window,
                cx,
            );
        };
        if guarded(
            gate,
            &self.shared.cfg,
            "Create index",
            statement,
            window,
            cx,
            run.clone(),
        ) {
            run(window, cx);
        }
    }
}

impl Render for IndexDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = tokens(cx);
        let mono = cx.theme().mono_font_family.clone();
        let (preview, ready) = match self.statement(cx) {
            Ok(statement) => (statement, true),
            Err(error) => (format!("-- {error:#}"), false),
        };
        v_flex()
            .gap(dp(14.))
            .child(dialog::title(
                format!("New index · {}", self.object.title()),
                None,
                cx,
            ))
            .child(dialog::section("NAME", Input::new(&self.index_name), cx))
            .child(dialog::section(
                "COLUMNS, IN ORDER",
                div()
                    .id("index-columns")
                    .max_h(dp(200.))
                    .overflow_y_scroll()
                    .child(
                        v_flex()
                            .gap(dp(4.))
                            .children(self.columns.iter().enumerate().map(|(index, column)| {
                                let order = self
                                    .chosen
                                    .iter()
                                    .position(|chosen| chosen == column)
                                    .map(|position| format!("{}", position + 1));
                                let column_name = column.clone();
                                h_flex()
                                    .gap(dp(8.))
                                    .child(
                                        Checkbox::new(("index-column", index))
                                            .cursor_pointer()
                                            .label(column.clone())
                                            .checked(order.is_some())
                                            .on_click(cx.listener(
                                                move |dialog, _: &bool, window, cx| {
                                                    dialog.toggle_column(&column_name, window, cx)
                                                },
                                            )),
                                    )
                                    .children(order.map(|order| {
                                        div()
                                            .text_size(dp(11.))
                                            .text_color(t.text_muted)
                                            .child(order)
                                    }))
                            })),
                    ),
                cx,
            ))
            .child(
                Checkbox::new("index-unique")
                    .cursor_pointer()
                    .label("Unique: no two rows may share these values")
                    .checked(self.unique)
                    .on_click(cx.listener(|dialog, checked: &bool, window, cx| {
                        dialog.unique = *checked;
                        dialog.update_name(window, cx);
                        cx.notify();
                    })),
            )
            .child(
                div()
                    .p(dp(12.))
                    .rounded(dp(8.))
                    .border_1()
                    .border_color(t.border)
                    .bg(t.canvas)
                    .font_family(mono)
                    .text_size(dp(11.5))
                    .text_color(t.text_secondary)
                    .child(preview),
            )
            .child(dialog::footer(
                "Building an index on a big table can take a while.",
                vec![
                    Button::new("cancel-index")
                        .cursor_pointer()
                        .label("Cancel")
                        .outline()
                        .on_click(|_, window, cx| window.close_dialog(cx))
                        .into_any_element(),
                    Button::new("create-index")
                        .cursor_pointer()
                        .label("Create index")
                        .primary()
                        .disabled_when(!ready)
                        .on_click(cx.listener(|dialog, _, window, cx| dialog.create(window, cx)))
                        .into_any_element(),
                ],
                cx,
            ))
    }
}
