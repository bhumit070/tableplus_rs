//! New Table / Edit Columns dialog: one row per column (name, type,
//! NULL, default, primary key), Preview SQL, then Apply. Edits of an
//! existing table become ALTER statements; nothing runs before Apply.

use std::rc::Rc;

use gpui::{
    App, AppContext as _, Context, Entity, IntoElement, ParentElement, Render, SharedString,
    Styled, Window, div, prelude::*, px,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::checkbox::Checkbox;
use gpui_component::dialog::DialogFooter;
use gpui_component::input::{Input, InputState};
use gpui_component::notification::Notification;
use gpui_component::{ActiveTheme as _, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit_assets::IconName;

use super::Shared;
use crate::db::ddl::{self, ColChange, ColDef};
use crate::db::{ColumnInfo, DbObject, Driver};
use crate::runtime;
use crate::ui::DisabledWhen as _;

struct Row {
    /// The column as it is now; `None` for a new one.
    orig: Option<ColDef>,
    name: Entity<InputState>,
    ty: Entity<InputState>,
    default: Entity<InputState>,
    nullable: bool,
    pk: bool,
    dropped: bool,
}

/// Called after the change ran, to reload the sidebar / tab.
pub type OnDone = Rc<dyn Fn(&mut Window, &mut App)>;

pub struct TableEditor {
    shared: Rc<Shared>,
    /// Editing this table; `None` = creating one.
    object: Option<DbObject>,
    schema: Option<String>,
    table_name: Entity<InputState>,
    rows: Vec<Row>,
    error: Option<String>,
    running: bool,
    on_done: OnDone,
}

impl TableEditor {
    fn row_mut(&mut self, key: gpui::EntityId) -> Option<&mut Row> {
        self.rows.iter_mut().find(|r| r.name.entity_id() == key)
    }
}

fn default_type(d: Driver) -> &'static str {
    match d {
        Driver::MySql => "varchar(255)",
        _ => "text",
    }
}

impl TableEditor {
    /// `object` = edit its columns; `None` = new table in `schema`.
    pub fn open(
        shared: Rc<Shared>,
        object: Option<DbObject>,
        columns: Vec<ColumnInfo>,
        schema: Option<String>,
        on_done: OnDone,
        window: &mut Window,
        cx: &mut App,
    ) -> Entity<TableEditor> {
        let title = match &object {
            Some(o) => format!("Edit Columns · {}", o.title()),
            None => "New Table".to_string(),
        };
        let view = cx.new(|cx| {
            let table_name = cx.new(|cx| InputState::new(window, cx).placeholder("table_name"));
            let mut ed = TableEditor {
                schema: object.as_ref().and_then(|o| o.schema.clone()).or(schema),
                shared,
                object,
                table_name,
                rows: vec![],
                error: None,
                running: false,
                on_done,
            };
            if ed.object.is_some() {
                for c in &columns {
                    ed.push_row(Some(ColDef::from_info(c)), window, cx);
                }
            } else {
                let int = if ed.shared.cfg.driver == Driver::Postgres {
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
                ed.push_row(None, window, cx);
                let r = ed.rows.last_mut().unwrap();
                r.pk = id.pk;
                r.nullable = id.nullable;
                let (n, t) = (r.name.clone(), r.ty.clone());
                n.update(cx, |i, cx| i.set_value(id.name, window, cx));
                t.update(cx, |i, cx| i.set_value(id.ty, window, cx));
            }
            ed
        });
        let first = std::cell::Cell::new(true);
        let result = view.clone();
        window.open_dialog(cx, move |dialog, window, cx| {
            if first.replace(false) && view.read(cx).object.is_none() {
                let name = view.read(cx).table_name.clone();
                name.update(cx, |n, cx| n.focus(window, cx));
            }
            let (v1, v2) = (view.clone(), view.clone());
            dialog
                .title(title.clone())
                .w(px(860.))
                .child(view.clone())
                .footer(
                    DialogFooter::new()
                        .child(
                            Button::new("preview-ddl")
                                .cursor_pointer()
                                .label("Preview SQL")
                                .small()
                                .on_click(move |_, window, cx| {
                                    v1.update(cx, |e, cx| e.preview(window, cx));
                                }),
                        )
                        .child(
                            Button::new("cancel-ddl")
                                .cursor_pointer()
                                .label("Cancel")
                                .small()
                                .on_click(|_, w, cx| w.close_dialog(cx)),
                        )
                        .child(
                            Button::new("apply-ddl")
                                .cursor_pointer()
                                .label("Apply")
                                .primary()
                                .small()
                                .on_click(move |_, window, cx| {
                                    v2.update(cx, |e, cx| e.apply(window, cx));
                                }),
                        ),
                )
        });
        result
    }

    fn push_row(&mut self, orig: Option<ColDef>, window: &mut Window, cx: &mut Context<Self>) {
        let driver = self.shared.cfg.driver;
        let o = orig.clone().unwrap_or(ColDef {
            name: String::new(),
            ty: String::new(),
            nullable: true,
            default: None,
            pk: false,
        });
        let input = |v: &str, ph: &str, window: &mut Window, cx: &mut Context<Self>| {
            let (v, ph) = (v.to_string(), ph.to_string());
            cx.new(|cx| InputState::new(window, cx).default_value(v).placeholder(ph))
        };
        let row = Row {
            name: input(&o.name, "column", window, cx),
            ty: input(&o.ty, default_type(driver), window, cx),
            default: input(o.default.as_deref().unwrap_or(""), "no default", window, cx),
            nullable: o.nullable,
            pk: o.pk,
            dropped: false,
            orig,
        };
        self.rows.push(row);
    }

    fn defs(&self, cx: &App) -> Vec<(Option<ColDef>, ColDef, bool)> {
        let driver = self.shared.cfg.driver;
        self.rows
            .iter()
            .map(|r| {
                let v = |e: &Entity<InputState>| e.read(cx).value().trim().to_string();
                let ty = v(&r.ty);
                let def = ColDef {
                    name: v(&r.name),
                    ty: if ty.is_empty() && r.orig.is_none() {
                        default_type(driver).into()
                    } else {
                        ty
                    },
                    nullable: r.nullable,
                    default: Some(v(&r.default)).filter(|d| !d.is_empty()),
                    pk: r.pk,
                };
                (r.orig.clone(), def, r.dropped)
            })
            .collect()
    }

    /// The statements Apply would run.
    fn statements(&self, cx: &App) -> anyhow::Result<Vec<String>> {
        let driver = self.shared.cfg.driver;
        let schema = self.schema.as_deref();
        let defs = self.defs(cx);
        match &self.object {
            None => {
                let cols: Vec<ColDef> = defs
                    .into_iter()
                    .filter(|(_, d, dropped)| !dropped && !d.name.is_empty())
                    .map(|(_, d, _)| d)
                    .collect();
                let name = self.table_name.read(cx).value().trim().to_string();
                Ok(vec![ddl::create_table(driver, schema, &name, &cols)?])
            }
            Some(obj) => {
                let mut changes = vec![];
                for (orig, def, dropped) in defs {
                    match (orig, dropped) {
                        (Some(o), true) => changes.push(ColChange::Drop(o.name)),
                        (Some(o), false) => changes.push(ColChange::Alter { from: o, to: def }),
                        (None, false) if !def.name.is_empty() => changes.push(ColChange::Add(def)),
                        _ => {}
                    }
                }
                ddl::alter_table(driver, schema, &obj.name, &changes)
            }
        }
    }

    fn preview(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.statements(cx) {
            Ok(s) if s.is_empty() => window.push_notification("No changes", cx),
            Ok(s) => {
                let sql: SharedString = s.join("\n").into();
                window
                    .open_alert_dialog(cx, move |d, _, _| d.title("SQL").description(sql.clone()));
            }
            Err(e) => {
                self.error = Some(format!("{e:#}"));
                cx.notify();
            }
        }
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

    #[cfg(test)]
    pub fn drop_row(&mut self, i: usize) {
        self.rows[i].dropped = true;
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
            self.push_row(None, window, cx);
        }
        let (n, t) = (self.rows[i].name.clone(), self.rows[i].ty.clone());
        n.update(cx, |x, cx| x.set_value(name.to_string(), window, cx));
        t.update(cx, |x, cx| x.set_value(ty.to_string(), window, cx));
    }
}

impl Render for TableEditor {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let header = |s: &'static str, w: f32| {
            div()
                .w(px(w))
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(s)
        };
        v_flex()
            .gap_2()
            .when(self.object.is_none(), |d| {
                d.child(
                    h_flex()
                        .gap_2()
                        .child(div().w(px(80.)).text_sm().child("Table name"))
                        .child(div().flex_1().child(Input::new(&self.table_name).small())),
                )
            })
            .child(
                h_flex()
                    .gap_2()
                    .child(header("Column", 200.))
                    .child(header("Type", 180.))
                    .child(header("NULL", 50.))
                    .child(header("Default", 200.))
                    .child(header("Key", 50.)),
            )
            .child(
                div()
                    .id("table-editor-rows")
                    .max_h(px(380.))
                    .overflow_y_scroll()
                    .child(v_flex().gap_1().children(self.rows.iter().enumerate().map(|(i, r)| {
                        // Clicks find their row by this id: positions shift
                        // when a row is removed before the next redraw.
                        let key = r.name.entity_id();
                        h_flex()
                            .gap_2()
                            .when(r.dropped, |d| d.opacity(0.45))
                            .child(div().w(px(200.)).child(Input::new(&r.name).small()))
                            .child(div().w(px(180.)).child(Input::new(&r.ty).small()))
                            .child(
                                div().w(px(50.)).child(
                                    Checkbox::new(("null", i)).cursor_pointer().checked(r.nullable).on_click(
                                        cx.listener(move |e, v: &bool, _, cx| {
                                            if let Some(r) = e.row_mut(key) {
                                                r.nullable = *v;
                                            }
                                            cx.notify();
                                        }),
                                    ),
                                ),
                            )
                            .child(div().w(px(200.)).child(Input::new(&r.default).small()))
                            .child(
                                div().w(px(50.)).child(
                                    // Only new tables set the key here.
                                    Checkbox::new(("pk", i)).cursor_pointer()
                                        .checked(r.pk)
                                        .disabled_when(self.object.is_some())
                                        .on_click(cx.listener(move |e, v: &bool, _, cx| {
                                            if let Some(r) = e.row_mut(key) {
                                                r.pk = *v;
                                                if *v {
                                                    r.nullable = false;
                                                }
                                            }
                                            cx.notify();
                                        })),
                                ),
                            )
                            .child(
                                Button::new(("drop-col", i)).cursor_pointer()
                                    .icon(if r.dropped { IconName::Undo } else { IconName::Delete })
                                    .ghost()
                                    .xsmall()
                                    .tooltip(if r.dropped { "Keep column" } else { "Remove column" })
                                    .on_click(cx.listener(move |e, _, _, cx| {
                                        let Some(i) =
                                            e.rows.iter().position(|r| r.name.entity_id() == key)
                                        else {
                                            return;
                                        };
                                        if e.rows[i].orig.is_none() {
                                            e.rows.remove(i);
                                        } else {
                                            e.rows[i].dropped = !e.rows[i].dropped;
                                        }
                                        cx.notify();
                                    })),
                            )
                    }))),
            )
            .child(
                h_flex().child(
                    Button::new("add-col").cursor_pointer()
                        .icon(IconName::Plus)
                        .label("Add Column")
                        .ghost()
                        .small()
                        .on_click(cx.listener(|e, _, window, cx| {
                            e.push_row(None, window, cx);
                            cx.notify();
                        })),
                ),
            )
            .when(self.shared.cfg.driver == Driver::Sqlite && self.object.is_some(), |d| {
                d.child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child("SQLite can add, rename and remove columns; other changes need a new table."),
                )
            })
            .when_some(self.error.clone(), |d, e| {
                d.child(div().text_sm().text_color(theme.danger).child(e))
            })
    }
}
