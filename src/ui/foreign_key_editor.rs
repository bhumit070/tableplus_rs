//! Structure › Foreign Keys (design.md screen 08): the table's foreign keys
//! as a list with a right-click menu (Copy definition, Drop foreign key…)
//! and the `+ Foreign key` dialog. SQLite can't change constraints of an
//! existing table, so there the list is read-only.

use std::cell::Cell;
use std::rc::Rc;

use gpui::{
    AnyElement, App, AppContext as _, ClipboardItem, Context, Entity, FontWeight, IntoElement,
    ParentElement, Render, Styled, Window, div, prelude::*,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::checkbox::Checkbox;
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::menu::{ContextMenuExt as _, DropdownMenu as _, PopupMenuItem};
use gpui_component::{ActiveTheme as _, Icon, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit_assets::IconName;

use super::confirm::ConfirmWrite;
use super::index_editor::run_statement;
use super::table_editor::OnDone;
use super::theme::{dp, tokens};
use super::{DisabledWhen as _, Shared, dialog, guarded};
use crate::db::ddl::{ON_DELETE, add_foreign_key, drop_foreign_key};
use crate::db::safety::gate_write;
use crate::db::{DbObject, Driver, ForeignKey, ObjectKind};
use crate::runtime;

/// `(customer_id) → customers (id) ON DELETE CASCADE`.
pub fn describe(key: &ForeignKey) -> String {
    let table = match &key.ref_schema {
        Some(schema) if !schema.is_empty() => format!("{schema}.{}", key.ref_table),
        _ => key.ref_table.clone(),
    };
    let mut text = format!(
        "({}) → {table} ({})",
        key.columns.join(", "),
        key.ref_columns.join(", ")
    );
    if !key.on_delete.is_empty() {
        text.push_str(&format!(" ON DELETE {}", key.on_delete));
    }
    text
}

/// The foreign-key list; `editable` adds `+ Foreign key` and Drop.
pub fn render_list(
    shared: &Rc<Shared>,
    object: &DbObject,
    keys: &[ForeignKey],
    on_add: Rc<dyn Fn(&mut Window, &mut App)>,
    on_done: OnDone,
    cx: &App,
) -> AnyElement {
    let t = tokens(cx);
    let mono = cx.theme().mono_font_family.clone();
    let editable = shared.cfg.driver != Driver::Sqlite;
    v_flex()
        .size_full()
        .child(
            h_flex()
                .gap(dp(10.))
                .px(dp(8.))
                .py(dp(6.))
                .when(editable, |bar| {
                    bar.child(
                        div().debug_selector(|| "add-foreign-key".into()).child(
                            Button::new("add-foreign-key")
                                .cursor_pointer()
                                .icon(IconName::Plus)
                                .label("Foreign key")
                                .ghost()
                                .small()
                                .on_click(move |_, window, cx| on_add(window, cx)),
                        ),
                    )
                })
                .when(!editable, |bar| {
                    bar.child(
                        div()
                            .px(dp(6.))
                            .text_size(dp(12.))
                            .text_color(t.text_muted)
                            .child("SQLite can't add or drop foreign keys on an existing table."),
                    )
                }),
        )
        .when(keys.is_empty(), |list| {
            list.child(
                div()
                    .px(dp(14.))
                    .text_size(dp(12.5))
                    .text_color(t.text_muted)
                    .child("No foreign keys."),
            )
        })
        .child(
            div()
                .id("foreign-key-list")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .children(keys.iter().enumerate().map(|(index, key)| {
                    let (shared, object, key_for_menu, on_done) =
                        (shared.clone(), object.clone(), key.clone(), on_done.clone());
                    h_flex()
                        .id(("foreign-key-row", index))
                        .gap(dp(12.))
                        .px(dp(14.))
                        .py(dp(8.))
                        .border_b_1()
                        .border_color(t.border)
                        .hover(|row| row.bg(t.hover))
                        .child(Icon::new(IconName::Link).small().text_color(t.text_muted))
                        .child(
                            div()
                                .w(dp(240.))
                                .flex_none()
                                .truncate()
                                .font_weight(FontWeight::MEDIUM)
                                .text_size(dp(13.))
                                .child(key.name.clone()),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .font_family(mono.clone())
                                .text_size(dp(12.))
                                .text_color(t.text_secondary)
                                .child(describe(key)),
                        )
                        .context_menu(move |menu, _, _| {
                            let definition = describe(&key_for_menu);
                            let name = key_for_menu.name.clone();
                            let (shared, object, on_done) =
                                (shared.clone(), object.clone(), on_done.clone());
                            let menu = menu.item(PopupMenuItem::new("Copy Definition").on_click(
                                move |_, _, cx| {
                                    cx.write_to_clipboard(ClipboardItem::new_string(
                                        definition.clone(),
                                    ))
                                },
                            ));
                            if !editable {
                                return menu;
                            }
                            menu.separator()
                                .item(PopupMenuItem::new("Drop Foreign Key…").on_click(
                                    move |_, window, cx| {
                                        confirm_drop(
                                            shared.clone(),
                                            &object,
                                            &name,
                                            on_done.clone(),
                                            window,
                                            cx,
                                        )
                                    },
                                ))
                        })
                })),
        )
        .into_any_element()
}

/// Dropping always asks first, whatever the safe mode.
pub fn confirm_drop(
    shared: Rc<Shared>,
    object: &DbObject,
    key_name: &str,
    on_done: OnDone,
    window: &mut Window,
    cx: &mut App,
) {
    let Ok(statement) = drop_foreign_key(
        shared.cfg.driver,
        object.schema.as_deref(),
        &object.name,
        key_name,
    ) else {
        return;
    };
    let cfg = shared.cfg.clone();
    let to_run = statement.clone();
    ConfirmWrite::open(
        &cfg,
        "Drop foreign key",
        statement,
        window,
        cx,
        Rc::new(move |window, cx| {
            run_statement(
                shared.clone(),
                to_run.clone(),
                "Foreign key dropped",
                on_done.clone(),
                window,
                cx,
            )
        }),
    );
}

/// The `+ Foreign key` dialog: one column, the table and column it refers
/// to, and what deleting the referenced row does.
pub struct ForeignKeyDialog {
    shared: Rc<Shared>,
    object: DbObject,
    columns: Vec<String>,
    column: Option<String>,
    ref_table: Entity<InputState>,
    ref_column: Entity<InputState>,
    key_name: Entity<InputState>,
    /// The name follows the column and table until it's typed over.
    name_typed: bool,
    on_delete: usize,
    /// Tables to pick from, once listed.
    tables: Vec<String>,
    on_done: OnDone,
}

impl ForeignKeyDialog {
    pub fn open(
        shared: Rc<Shared>,
        object: DbObject,
        columns: Vec<String>,
        on_done: OnDone,
        window: &mut Window,
        cx: &mut App,
    ) -> Entity<ForeignKeyDialog> {
        let view = cx.new(|cx| {
            let input =
                |placeholder: &str, value: &str, window: &mut Window, cx: &mut Context<Self>| {
                    let (placeholder, value) = (placeholder.to_string(), value.to_string());
                    cx.new(|cx| {
                        InputState::new(window, cx)
                            .placeholder(placeholder)
                            .default_value(value)
                    })
                };
            let key_name = input("fk_name", "", window, cx);
            let ref_table = input("table", "", window, cx);
            let ref_column = input("column", "id", window, cx);
            cx.subscribe(&key_name, |dialog: &mut ForeignKeyDialog, _, event, cx| {
                if let InputEvent::Change = event {
                    dialog.name_typed = true;
                    cx.notify();
                }
            })
            .detach();
            for follows in [&ref_table, &ref_column] {
                cx.subscribe_in(
                    follows,
                    window,
                    |dialog: &mut ForeignKeyDialog, _, event, window, cx| {
                        if let InputEvent::Change = event {
                            dialog.update_name(window, cx);
                            cx.notify();
                        }
                    },
                )
                .detach();
            }
            ForeignKeyDialog {
                shared: shared.clone(),
                object,
                columns,
                column: None,
                ref_table,
                ref_column,
                key_name,
                name_typed: false,
                on_delete: 0,
                tables: vec![],
                on_done,
            }
        });
        view.update(cx, |dialog, cx| dialog.list_tables(cx));
        let first = Cell::new(true);
        let result = view.clone();
        let width = dialog::width(560., cx);
        window.open_dialog(cx, move |dialog_box, window, cx| {
            if first.replace(false) {
                let table = view.read(cx).ref_table.clone();
                table.update(cx, |input, cx| input.focus(window, cx));
            }
            dialog::shell(dialog_box, width, cx).child(view.clone())
        });
        result
    }

    fn list_tables(&mut self, cx: &mut Context<Self>) {
        let conn = self.shared.conn.clone();
        let task = runtime::spawn(async move { conn.read().await.list_objects().await });
        cx.spawn(async move |this, cx| {
            let objects = task.await.unwrap_or_default();
            this.update(cx, |dialog, cx| {
                dialog.tables = objects
                    .into_iter()
                    .filter(|object| object.kind == ObjectKind::Table)
                    .map(|object| object.name)
                    .collect();
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// `orders_customer_id_fkey`, for the chosen column.
    fn update_name(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.name_typed {
            return;
        }
        let name = match &self.column {
            Some(column) => format!("{}_{column}_fkey", self.object.name),
            None => String::new(),
        };
        self.key_name
            .update(cx, |input, cx| input.set_value(name, window, cx));
        // Setting it isn't the user typing.
        self.name_typed = false;
    }

    pub fn choose_column(&mut self, column: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.column = Some(column.to_string());
        self.update_name(window, cx);
        cx.notify();
    }

    #[cfg(test)]
    pub fn set_reference(
        &mut self,
        table: &str,
        column: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.ref_table.update(cx, |input, cx| {
            input.set_value(table.to_string(), window, cx)
        });
        self.ref_column.update(cx, |input, cx| {
            input.set_value(column.to_string(), window, cx)
        });
    }

    pub fn set_on_delete(&mut self, index: usize, cx: &mut Context<Self>) {
        self.on_delete = index.min(ON_DELETE.len() - 1);
        cx.notify();
    }

    fn key(&self, cx: &App) -> ForeignKey {
        ForeignKey {
            name: self.key_name.read(cx).value().trim().to_string(),
            columns: self.column.iter().cloned().collect(),
            ref_schema: None,
            ref_table: self.ref_table.read(cx).value().trim().to_string(),
            ref_columns: vec![self.ref_column.read(cx).value().trim().to_string()],
            on_update: String::new(),
            on_delete: ON_DELETE[self.on_delete].to_string(),
        }
    }

    fn statement(&self, cx: &App) -> anyhow::Result<String> {
        add_foreign_key(
            self.shared.cfg.driver,
            self.object.schema.as_deref(),
            &self.object.name,
            &self.key(cx),
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
                "Foreign key added",
                on_done.clone(),
                window,
                cx,
            );
        };
        if guarded(
            gate,
            &self.shared.cfg,
            "Add foreign key",
            statement,
            window,
            cx,
            run.clone(),
        ) {
            run(window, cx);
        }
    }
}

impl Render for ForeignKeyDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = tokens(cx);
        let mono = cx.theme().mono_font_family.clone();
        let (preview, ready) = match self.statement(cx) {
            Ok(statement) => (statement, true),
            Err(error) => (format!("-- {error:#}"), false),
        };
        let tables = self.tables.clone();
        let ref_table = self.ref_table.clone();
        let me = cx.entity();
        v_flex()
            .gap(dp(14.))
            .child(dialog::title(
                format!("New foreign key · {}", self.object.title()),
                None,
                cx,
            ))
            .child(dialog::section(
                "COLUMN",
                div()
                    .id("foreign-key-columns")
                    .max_h(dp(160.))
                    .overflow_y_scroll()
                    .child(
                        v_flex()
                            .gap(dp(4.))
                            .children(self.columns.iter().enumerate().map(|(index, column)| {
                                let column_name = column.clone();
                                Checkbox::new(("foreign-key-column", index))
                                    .cursor_pointer()
                                    .label(column.clone())
                                    .checked(self.column.as_deref() == Some(column.as_str()))
                                    .on_click(cx.listener(move |dialog, _: &bool, window, cx| {
                                        dialog.choose_column(&column_name, window, cx)
                                    }))
                            })),
                    ),
                cx,
            ))
            .child(dialog::section(
                "REFERENCES",
                h_flex()
                    .gap(dp(8.))
                    .child(
                        h_flex()
                            .flex_1()
                            .gap(dp(2.))
                            .child(div().flex_1().child(Input::new(&self.ref_table)))
                            .child(
                                Button::new("foreign-key-tables")
                                    .cursor_pointer()
                                    .icon(IconName::ChevronDown)
                                    .ghost()
                                    .small()
                                    .disabled_when(tables.is_empty())
                                    .dropdown_menu(move |menu, _, _| {
                                        tables.iter().fold(menu, |menu, table| {
                                            let (table, input) = (table.clone(), ref_table.clone());
                                            menu.item(PopupMenuItem::new(table.clone()).on_click(
                                                move |_, window, cx| {
                                                    input.update(cx, |input, cx| {
                                                        input.set_value(table.clone(), window, cx)
                                                    })
                                                },
                                            ))
                                        })
                                    }),
                            ),
                    )
                    .child(div().text_color(t.text_muted).child("."))
                    .child(div().w(dp(150.)).child(Input::new(&self.ref_column))),
                cx,
            ))
            .child(dialog::section(
                "ON DELETE",
                dialog::segmented(
                    "foreign-key-on-delete",
                    &ON_DELETE,
                    self.on_delete,
                    Rc::new(move |index, _, cx| {
                        me.update(cx, |dialog, cx| dialog.set_on_delete(index, cx))
                    }),
                    cx,
                ),
                cx,
            ))
            .child(dialog::section("NAME", Input::new(&self.key_name), cx))
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
                "Existing rows must already match, or adding it fails.",
                vec![
                    Button::new("cancel-foreign-key")
                        .cursor_pointer()
                        .label("Cancel")
                        .outline()
                        .on_click(|_, window, cx| window.close_dialog(cx))
                        .into_any_element(),
                    Button::new("create-foreign-key")
                        .cursor_pointer()
                        .label("Add foreign key")
                        .primary()
                        .disabled_when(!ready)
                        .on_click(cx.listener(|dialog, _, window, cx| dialog.create(window, cx)))
                        .into_any_element(),
                ],
                cx,
            ))
    }
}
