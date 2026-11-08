//! New table dialog (design.md screen 18): one row per column (name, type
//! with a menu of common types, NULL, default, primary key) and the
//! CREATE TABLE it makes, live. A new table can start blank, with id +
//! timestamps, or as a copy of another table's columns. Existing tables
//! are edited in place (`structure_editor.rs`).

use std::rc::Rc;

use std::cell::Cell;

use gpui::{
    App, AppContext as _, Context, Entity, FontWeight, IntoElement, ParentElement, Render, Styled,
    Window, div, prelude::*,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::checkbox::Checkbox;
use gpui_component::input::{Input, InputState};
use gpui_component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_component::notification::Notification;
use gpui_component::select::{Select, SelectEvent, SelectState};
use gpui_component::{ActiveTheme as _, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit_assets::IconName;

use super::theme::{dp, tokens};
use super::{SearchList, Shared, dialog, search_list};
use crate::db::ddl::{self, ColDef};
use crate::db::{DbObject, Driver, ObjectKind};
use crate::runtime;
use anyhow::Result;
use gpui::EntityId;

struct Row {
    name: Entity<InputState>,
    ty: Entity<InputState>,
    default: Entity<InputState>,
    nullable: bool,
    pk: bool,
}

/// Called after the change ran, to reload the sidebar / tab.
pub type OnDone = Rc<dyn Fn(&mut Window, &mut App)>;

/// What a new table starts from.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Start {
    Blank,
    Timestamps,
    CopyOf,
}

pub struct TableEditor {
    shared: Rc<Shared>,
    start: Start,
    /// "Copy of…": the tables to copy from, once listed.
    copy_from: Option<Entity<SelectState<SearchList>>>,
    copy_tables: Vec<DbObject>,
    schema: Option<String>,
    table_name: Entity<InputState>,
    rows: Vec<Row>,
    error: Option<String>,
    running: bool,
    on_done: OnDone,
}

impl TableEditor {
    fn row_mut(&mut self, key: EntityId) -> Option<&mut Row> {
        self.rows.iter_mut().find(|r| r.name.entity_id() == key)
    }
}

pub(super) fn default_type(d: Driver) -> &'static str {
    match d {
        Driver::MySql => "varchar(255)",
        _ => "text",
    }
}

impl TableEditor {
    /// A new table in `schema`.
    pub fn open(
        shared: Rc<Shared>,
        schema: Option<String>,
        on_done: OnDone,
        window: &mut Window,
        cx: &mut App,
    ) -> Entity<TableEditor> {
        let view = cx.new(|cx| {
            let table_name = cx.new(|cx| InputState::new(window, cx).placeholder("table_name"));
            let mut ed = TableEditor {
                start: Start::Blank,
                copy_from: None,
                copy_tables: vec![],
                schema,
                shared,
                table_name,
                rows: vec![],
                error: None,
                running: false,
                on_done,
            };
            ed.apply_start(Start::Blank, window, cx);
            ed
        });
        let first = Cell::new(true);
        let result = view.clone();
        let width = dialog::width(880., cx);
        window.open_dialog(cx, move |d, window, cx| {
            if first.replace(false) {
                let name = view.read(cx).table_name.clone();
                name.update(cx, |n, cx| n.focus(window, cx));
            }
            dialog::shell(d, width, cx).child(view.clone())
        });
        result
    }

    /// Replaces the columns of a new table with `start`'s.
    fn apply_start(&mut self, start: Start, window: &mut Window, cx: &mut Context<Self>) {
        self.start = start;
        let driver = self.shared.cfg.driver;
        let int = if driver == Driver::Postgres {
            "bigint"
        } else {
            "integer"
        };
        let id = ColDef {
            name: "id".into(),
            ty: int.into(),
            nullable: false,
            default: None,
            pk: true,
        };
        let (stamp, now) = match driver {
            Driver::Postgres => ("timestamptz", "now()"),
            Driver::MySql => ("datetime", "CURRENT_TIMESTAMP"),
            _ => ("text", "CURRENT_TIMESTAMP"),
        };
        let stamp = |name: &str| ColDef {
            name: name.into(),
            ty: stamp.into(),
            nullable: false,
            default: Some(now.into()),
            pk: false,
        };
        let columns = match start {
            Start::Blank => vec![id],
            Start::Timestamps => vec![id, stamp("created_at"), stamp("updated_at")],
            Start::CopyOf => {
                self.list_tables(window, cx);
                return;
            }
        };
        self.copy_from = None;
        self.set_columns(columns, window, cx);
    }

    /// New columns (not from an existing table) in place of the current
    /// ones.
    fn set_columns(&mut self, columns: Vec<ColDef>, window: &mut Window, cx: &mut Context<Self>) {
        self.rows.clear();
        for column in columns {
            self.push_row(window, cx);
            let row = self.rows.last_mut().expect("just pushed");
            row.pk = column.pk;
            row.nullable = column.nullable;
            let (name, ty, default) = (row.name.clone(), row.ty.clone(), row.default.clone());
            name.update(cx, |input, cx| input.set_value(column.name, window, cx));
            ty.update(cx, |input, cx| input.set_value(column.ty, window, cx));
            if let Some(value) = column.default {
                default.update(cx, |input, cx| input.set_value(value, window, cx));
            }
        }
        cx.notify();
    }

    /// "Copy of…": list the tables to pick from.
    fn list_tables(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let conn = self.shared.conn.clone();
        let task = runtime::spawn(async move { conn.read().await.list_objects().await });
        cx.spawn_in(window, async move |this, cx| {
            let listed = task.await;
            this.update_in(cx, |editor, window, cx| {
                let tables: Vec<DbObject> = match listed {
                    Ok(objects) => objects
                        .into_iter()
                        .filter(|o| o.kind == ObjectKind::Table)
                        .collect(),
                    Err(e) => {
                        editor.error = Some(format!("{e:#}"));
                        cx.notify();
                        return;
                    }
                };
                let names = tables.iter().map(|t| t.title()).collect();
                let select = cx.new(|cx| {
                    SelectState::new(search_list(names), None, window, cx).searchable(true)
                });
                cx.subscribe_in(
                    &select,
                    window,
                    |editor, _, event: &SelectEvent<SearchList>, window, cx| {
                        if let SelectEvent::Confirm(Some(title)) = event {
                            editor.copy_columns_of(title.clone(), window, cx);
                        }
                    },
                )
                .detach();
                editor.copy_tables = tables;
                editor.copy_from = Some(select);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Takes the columns (and key) of the table titled `title`.
    fn copy_columns_of(&mut self, title: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(table) = self
            .copy_tables
            .iter()
            .find(|t| t.title() == title)
            .cloned()
        else {
            return;
        };
        let conn = self.shared.conn.clone();
        let task = runtime::spawn(async move { conn.read().await.structure(&table).await });
        cx.spawn_in(window, async move |this, cx| {
            let structure = task.await;
            this.update_in(cx, |editor, window, cx| match structure {
                Ok(structure) => {
                    let columns = structure.columns.iter().map(ColDef::from_info).collect();
                    editor.set_columns(columns, window, cx);
                }
                Err(e) => {
                    editor.error = Some(format!("{e:#}"));
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    fn push_row(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let driver = self.shared.cfg.driver;
        let input = |placeholder: &str, window: &mut Window, cx: &mut Context<Self>| {
            let placeholder = placeholder.to_string();
            cx.new(|cx| InputState::new(window, cx).placeholder(placeholder))
        };
        let row = Row {
            name: input("column", window, cx),
            ty: input(default_type(driver), window, cx),
            default: input("no default", window, cx),
            nullable: true,
            pk: false,
        };
        self.rows.push(row);
    }

    /// The named rows as columns (an empty type means the default one).
    fn defs(&self, cx: &App) -> Vec<ColDef> {
        let driver = self.shared.cfg.driver;
        self.rows
            .iter()
            .map(|row| {
                let value = |input: &Entity<InputState>| input.read(cx).value().trim().to_string();
                let ty = value(&row.ty);
                ColDef {
                    name: value(&row.name),
                    ty: if ty.is_empty() {
                        default_type(driver).into()
                    } else {
                        ty
                    },
                    nullable: row.nullable,
                    default: Some(value(&row.default)).filter(|default| !default.is_empty()),
                    pk: row.pk,
                }
            })
            .filter(|column| !column.name.is_empty())
            .collect()
    }

    /// The CREATE TABLE statement Create table would run.
    fn statements(&self, cx: &App) -> Result<Vec<String>> {
        let name = self.table_name.read(cx).value().trim().to_string();
        let columns = self.defs(cx);
        Ok(vec![ddl::create_table(
            self.shared.cfg.driver,
            self.schema.as_deref(),
            &name,
            &columns,
        )?])
    }

    fn apply(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.running {
            return;
        }
        let stmts = match self.statements(cx) {
            Ok(s) if s.is_empty() => {
                window.close_dialog(cx);
                return;
            }
            Ok(s) => s,
            Err(e) => {
                self.error = Some(format!("{e:#}"));
                cx.notify();
                return;
            }
        };
        let gate = crate::db::safety::gate_write(&self.shared.cfg);
        let detail = stmts.join("\n");
        let me = cx.entity().downgrade();
        let s2 = stmts.clone();
        if super::guarded(
            gate,
            &self.shared.cfg,
            "Change structure",
            detail,
            window,
            cx,
            move |w, cx| {
                me.update(cx, |e, cx| e.run(s2.clone(), w, cx)).ok();
            },
        ) {
            self.run(stmts, window, cx);
        }
    }

    fn run(&mut self, stmts: Vec<String>, window: &mut Window, cx: &mut Context<Self>) {
        self.running = true;
        self.error = None;
        cx.notify();
        let script = ddl::script(self.shared.cfg.driver, &stmts);
        let conn = self.shared.conn.clone();
        let task = runtime::spawn(async move {
            conn.read()
                .await
                .execute_cancellable(&script, &crate::db::Cancel::default())
                .await
        });
        let on_done = self.on_done.clone();
        cx.spawn_in(window, async move |this, cx| {
            let res = task.await;
            this.update_in(cx, |e, window, cx| {
                e.running = false;
                match res {
                    Ok(_) => {
                        window.close_dialog(cx);
                        window.push_notification(Notification::success("Structure updated"), cx);
                        on_done(window, cx);
                    }
                    Err(err) => e.error = Some(format!("{err:#}")),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    #[cfg(test)]
    pub fn apply_for_test(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.apply(window, cx);
    }

    #[cfg(test)]
    pub fn set_name(&mut self, name: &str, window: &mut Window, cx: &mut Context<Self>) {
        let n = self.table_name.clone();
        n.update(cx, |x, cx| x.set_value(name.to_string(), window, cx));
    }

    /// `None`: start from "id + timestamps"; `Some(title)`: "Copy of…" that
    /// table.
    #[cfg(test)]
    pub fn start_for_test(
        &mut self,
        copy: Option<&str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match copy {
            None => self.apply_start(Start::Timestamps, window, cx),
            Some(title) => {
                self.start = Start::CopyOf;
                self.copy_tables = vec![DbObject {
                    schema: None,
                    name: title.into(),
                    kind: ObjectKind::Table,
                }];
                self.copy_columns_of(title.into(), window, cx);
            }
        }
    }

    #[cfg(test)]
    pub fn column_count(&self) -> usize {
        self.rows.len()
    }

    #[cfg(test)]
    pub fn set_row(
        &mut self,
        i: usize,
        name: &str,
        ty: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        while self.rows.len() <= i {
            self.push_row(window, cx);
        }
        let (n, t) = (self.rows[i].name.clone(), self.rows[i].ty.clone());
        n.update(cx, |x, cx| x.set_value(name.to_string(), window, cx));
        t.update(cx, |x, cx| x.set_value(ty.to_string(), window, cx));
    }
}

/// Types offered by each row's type menu, most used first.
pub(super) fn common_types(driver: Driver) -> &'static [&'static str] {
    match driver {
        Driver::Postgres => &[
            "bigint",
            "integer",
            "text",
            "varchar(255)",
            "boolean",
            "numeric(12,2)",
            "timestamptz",
            "date",
            "uuid",
            "jsonb",
        ],
        Driver::MySql => &[
            "bigint",
            "int",
            "varchar(255)",
            "text",
            "tinyint(1)",
            "decimal(12,2)",
            "datetime",
            "date",
            "json",
        ],
        _ => &["integer", "text", "real", "numeric", "blob"],
    }
}

impl Render for TableEditor {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = tokens(cx);
        let mono = cx.theme().mono_font_family.clone();
        let driver = self.shared.cfg.driver;
        let header = |label: &'static str, width: f32| {
            div()
                .w(dp(width))
                .text_size(dp(11.))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(t.text_muted)
                .child(label)
        };
        // The DDL these rows make, as Apply would run it.
        let named = !self.table_name.read(cx).value().trim().is_empty();
        let ddl_text = match self.statements(cx) {
            Ok(statements) if statements.is_empty() => "-- no changes".to_string(),
            Ok(statements) => statements.join("\n"),
            Err(_) if !named => "-- name the table to see its CREATE TABLE".to_string(),
            Err(e) => format!("-- {e:#}"),
        };
        let me = cx.entity();
        let start_index = match self.start {
            Start::Blank => 0,
            Start::Timestamps => 1,
            Start::CopyOf => 2,
        };
        v_flex()
            .gap(dp(14.))
            .child(dialog::title("New table", None, cx))
            .child(
                h_flex()
                    .gap(dp(12.))
                    .items_end()
                    .child(
                        v_flex()
                            .flex_1()
                            .gap(dp(6.))
                            .child(header("NAME", 80.))
                            .child(Input::new(&self.table_name)),
                    )
                    .child(
                        v_flex()
                            .gap(dp(6.))
                            .child(header("START FROM", 120.))
                            .child(
                                dialog::segmented(
                                    "table-start",
                                    &["Blank", "id + timestamps", "Copy of…"],
                                    start_index,
                                    Rc::new(move |index, window, cx| {
                                        let start =
                                            [Start::Blank, Start::Timestamps, Start::CopyOf][index];
                                        me.update(cx, |editor, cx| {
                                            editor.apply_start(start, window, cx)
                                        })
                                    }),
                                    cx,
                                )
                                .w(dp(360.)),
                            ),
                    ),
            )
            .when_some(
                self.copy_from
                    .clone()
                    .filter(|_| self.start == Start::CopyOf),
                |column, select| {
                    column.child(
                        h_flex()
                            .gap(dp(8.))
                            .text_size(dp(12.))
                            .text_color(t.text_secondary)
                            .child("Columns of")
                            .child(
                                div()
                                    .w(dp(280.))
                                    .child(Select::new(&select).cursor_pointer().small()),
                            ),
                    )
                },
            )
            .child(
                h_flex()
                    .gap(dp(8.))
                    .child(header("COLUMN", 200.))
                    .child(header("TYPE", 210.))
                    .child(header("NULL", 50.))
                    .child(header("DEFAULT", 200.))
                    .child(header("KEY", 50.)),
            )
            .child(
                div()
                    .id("table-editor-rows")
                    .max_h(dp(300.))
                    .overflow_y_scroll()
                    .child(
                        v_flex()
                            .gap(dp(6.))
                            .children(self.rows.iter().enumerate().map(|(index, row)| {
                                // Clicks find their row by this id: positions shift
                                // when a row is removed before the next redraw.
                                let key = row.name.entity_id();
                                let ty = row.ty.clone();
                                h_flex()
                                    .gap(dp(8.))
                                    .child(div().w(dp(200.)).child(Input::new(&row.name).small()))
                                    .child(
                                        h_flex()
                                            .w(dp(210.))
                                            .gap(dp(2.))
                                            .child(
                                                div().flex_1().child(Input::new(&row.ty).small()),
                                            )
                                            .child(
                                                Button::new(("types", index))
                                                    .cursor_pointer()
                                                    .icon(IconName::ChevronDown)
                                                    .ghost()
                                                    .xsmall()
                                                    .dropdown_menu(move |menu, _, _| {
                                                        common_types(driver).iter().fold(
                                                            menu,
                                                            |menu, name| {
                                                                let ty = ty.clone();
                                                                menu.item(
                                                                    PopupMenuItem::new(*name)
                                                                        .on_click(
                                                                            move |_, window, cx| {
                                                                                ty.update(
                                                                            cx,
                                                                            |input, cx| {
                                                                                input.set_value(
                                                                                    *name, window,
                                                                                    cx,
                                                                                )
                                                                            },
                                                                        )
                                                                            },
                                                                        ),
                                                                )
                                                            },
                                                        )
                                                    }),
                                            ),
                                    )
                                    .child(
                                        div().w(dp(50.)).child(
                                            Checkbox::new(("null", index))
                                                .cursor_pointer()
                                                .checked(row.nullable)
                                                .on_click(cx.listener(
                                                    move |editor, on: &bool, _, cx| {
                                                        if let Some(row) = editor.row_mut(key) {
                                                            row.nullable = *on;
                                                        }
                                                        cx.notify();
                                                    },
                                                )),
                                        ),
                                    )
                                    .child(
                                        div().w(dp(200.)).child(Input::new(&row.default).small()),
                                    )
                                    .child(
                                        div().w(dp(50.)).child(
                                            // Only new tables set the key here.
                                            Checkbox::new(("pk", index))
                                                .cursor_pointer()
                                                .checked(row.pk)
                                                .on_click(cx.listener(
                                                    move |editor, on: &bool, _, cx| {
                                                        if let Some(row) = editor.row_mut(key) {
                                                            row.pk = *on;
                                                            if *on {
                                                                row.nullable = false;
                                                            }
                                                        }
                                                        cx.notify();
                                                    },
                                                )),
                                        ),
                                    )
                                    .child(
                                        Button::new(("drop-col", index))
                                            .cursor_pointer()
                                            .icon(IconName::Trash)
                                            .ghost()
                                            .xsmall()
                                            .tooltip("Remove column")
                                            .on_click(cx.listener(move |editor, _, _, cx| {
                                                let Some(index) = editor
                                                    .rows
                                                    .iter()
                                                    .position(|row| row.name.entity_id() == key)
                                                else {
                                                    return;
                                                };
                                                editor.rows.remove(index);
                                                cx.notify();
                                            })),
                                    )
                            })),
                    ),
            )
            .child(
                h_flex().child(
                    Button::new("add-col")
                        .cursor_pointer()
                        .icon(IconName::Plus)
                        .label("Add column")
                        .ghost()
                        .small()
                        .on_click(cx.listener(|editor, _, window, cx| {
                            editor.push_row(window, cx);
                            cx.notify();
                        })),
                ),
            )
            .child(
                div()
                    .id("table-editor-ddl")
                    .max_h(dp(140.))
                    .overflow_y_scroll()
                    .p(dp(12.))
                    .rounded(dp(8.))
                    .border_1()
                    .border_color(t.border)
                    .bg(t.canvas)
                    .font_family(mono)
                    .text_size(dp(11.5))
                    .text_color(t.text_secondary)
                    .children(
                        ddl_text
                            .lines()
                            .map(|line| div().child(line.to_string()))
                            .collect::<Vec<_>>(),
                    ),
            )
            .when_some(self.error.clone(), |column, e| {
                column.child(div().text_size(dp(12.)).text_color(t.danger).child(e))
            })
            .child(dialog::footer(
                "Nothing runs until you apply it.",
                vec![
                    Button::new("cancel-ddl")
                        .cursor_pointer()
                        .label("Cancel")
                        .outline()
                        .on_click(|_, window, cx| window.close_dialog(cx))
                        .into_any_element(),
                    Button::new("apply-ddl")
                        .cursor_pointer()
                        .label("Create table")
                        .primary()
                        .loading(self.running)
                        .on_click(cx.listener(|editor, _, window, cx| editor.apply(window, cx)))
                        .into_any_element(),
                ],
                cx,
            ))
    }
}
