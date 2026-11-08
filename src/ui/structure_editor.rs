//! Structure › Columns, edited in place (design.md screen 08): one row per
//! column (name, type with a menu of common types, NULL, default). Changes
//! stage with the data grid's colours (amber altered with "was …", green
//! new, red removed) and run only on Apply, as one script that's
//! transactional where the engine allows it.

use std::rc::Rc;

use gpui::{
    AnyElement, App, AppContext as _, Context, Entity, EntityId, FontWeight, Hsla, IntoElement,
    ParentElement, Render, Styled, Subscription, Window, div, prelude::*, transparent_black,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::checkbox::Checkbox;
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_component::notification::Notification;
use gpui_component::{ActiveTheme as _, Icon, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit_assets::IconName;

use super::table_editor::{OnDone, common_types, default_type};
use super::theme::{Tokens, dp, tokens};
use super::{Shared, guarded};
use crate::db::ddl::{self, ColChange, ColDef};
use crate::db::safety::gate_write;
use crate::db::{Cancel, ColumnInfo, DbObject, Driver};
use crate::runtime;
use anyhow::Result;

struct ColumnRow {
    /// The column as the server has it; `None` for one added here.
    original: Option<ColDef>,
    name: Entity<InputState>,
    data_type: Entity<InputState>,
    default: Entity<InputState>,
    nullable: bool,
    removed: bool,
}

/// How a row differs from the server's column.
#[derive(Clone, Copy, PartialEq, Eq)]
enum RowState {
    Unchanged,
    Altered,
    New,
    Removed,
}

pub struct StructureEditor {
    shared: Rc<Shared>,
    object: DbObject,
    /// The columns as loaded, for Discard.
    columns: Vec<ColumnInfo>,
    rows: Vec<ColumnRow>,
    show_ddl: bool,
    error: Option<String>,
    running: bool,
    on_done: OnDone,
    _subscriptions: Vec<Subscription>,
}

impl StructureEditor {
    pub fn new(
        shared: Rc<Shared>,
        object: DbObject,
        columns: Vec<ColumnInfo>,
        on_done: OnDone,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut editor = StructureEditor {
            shared,
            object,
            columns: vec![],
            rows: vec![],
            show_ddl: false,
            error: None,
            running: false,
            on_done,
            _subscriptions: vec![],
        };
        editor.reset(columns, window, cx);
        editor
    }

    /// Rows for `columns`, dropping anything staged.
    fn reset(&mut self, columns: Vec<ColumnInfo>, window: &mut Window, cx: &mut Context<Self>) {
        self.rows.clear();
        self._subscriptions.clear();
        for column in &columns {
            self.push_row(Some(ColDef::from_info(column)), window, cx);
        }
        self.columns = columns;
        self.error = None;
        self.show_ddl = false;
        cx.notify();
    }

    fn push_row(&mut self, original: Option<ColDef>, window: &mut Window, cx: &mut Context<Self>) {
        let driver = self.shared.cfg.driver;
        let shown = original.clone().unwrap_or(ColDef {
            name: String::new(),
            ty: String::new(),
            nullable: true,
            default: None,
            pk: false,
        });
        let mut input = |value: &str, placeholder: &str| {
            let (value, placeholder) = (value.to_string(), placeholder.to_string());
            let input = cx.new(|cx| {
                InputState::new(window, cx)
                    .default_value(value)
                    .placeholder(placeholder)
            });
            // Staged colours and the change count follow every keystroke.
            self._subscriptions
                .push(cx.subscribe(&input, |_, _, event, cx| {
                    if let InputEvent::Change = event {
                        cx.notify();
                    }
                }));
            input
        };
        let row = ColumnRow {
            name: input(&shown.name, "column"),
            data_type: input(&shown.ty, default_type(driver)),
            default: input(shown.default.as_deref().unwrap_or(""), "no default"),
            nullable: shown.nullable,
            removed: false,
            original,
        };
        self.rows.push(row);
    }

    fn row_mut(&mut self, key: EntityId) -> Option<&mut ColumnRow> {
        self.rows.iter_mut().find(|row| row.name.entity_id() == key)
    }

    /// The row as the inputs describe it now.
    fn definition(&self, row: &ColumnRow, cx: &App) -> ColDef {
        let value = |input: &Entity<InputState>| input.read(cx).value().trim().to_string();
        let data_type = value(&row.data_type);
        ColDef {
            name: value(&row.name),
            ty: if data_type.is_empty() && row.original.is_none() {
                default_type(self.shared.cfg.driver).into()
            } else {
                data_type
            },
            nullable: row.nullable,
            default: Some(value(&row.default)).filter(|default| !default.is_empty()),
            pk: row.original.as_ref().is_some_and(|original| original.pk),
        }
    }

    fn state(&self, row: &ColumnRow, cx: &App) -> RowState {
        match &row.original {
            None => RowState::New,
            Some(_) if row.removed => RowState::Removed,
            Some(original) => {
                let now = self.definition(row, cx);
                let same = now.name == original.name
                    && now.ty.eq_ignore_ascii_case(&original.ty)
                    && now.nullable == original.nullable
                    && now.default == original.default;
                if same {
                    RowState::Unchanged
                } else {
                    RowState::Altered
                }
            }
        }
    }

    /// Staged changes; a new row without a name doesn't count yet.
    pub fn change_count(&self, cx: &App) -> usize {
        self.rows
            .iter()
            .filter(|row| match self.state(row, cx) {
                RowState::Unchanged => false,
                RowState::New => !row.name.read(cx).value().trim().is_empty(),
                _ => true,
            })
            .count()
    }

    /// The statements Apply would run.
    fn statements(&self, cx: &App) -> Result<Vec<String>> {
        let mut changes = vec![];
        for row in &self.rows {
            let definition = self.definition(row, cx);
            match (&row.original, row.removed) {
                (Some(original), true) => changes.push(ColChange::Drop(original.name.clone())),
                (Some(original), false) => changes.push(ColChange::Alter {
                    from: original.clone(),
                    to: definition,
                }),
                (None, _) if !definition.name.is_empty() => {
                    changes.push(ColChange::Add(definition))
                }
                _ => {}
            }
        }
        ddl::alter_table(
            self.shared.cfg.driver,
            self.object.schema.as_deref(),
            &self.object.name,
            &changes,
        )
    }

    fn discard(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let columns = self.columns.clone();
        self.reset(columns, window, cx);
    }

    fn apply(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.running {
            return;
        }
        let statements = match self.statements(cx) {
            Ok(statements) if statements.is_empty() => return,
            Ok(statements) => statements,
            Err(error) => {
                self.error = Some(format!("{error:#}"));
                cx.notify();
                return;
            }
        };
        let gate = gate_write(&self.shared.cfg);
        let detail = statements.join("\n");
        let editor = cx.entity().downgrade();
        let confirmed = statements.clone();
        if guarded(
            gate,
            &self.shared.cfg,
            "Change structure",
            detail,
            window,
            cx,
            move |window, cx| {
                editor
                    .update(cx, |editor, cx| editor.run(confirmed.clone(), window, cx))
                    .ok();
            },
        ) {
            self.run(statements, window, cx);
        }
    }

    fn run(&mut self, statements: Vec<String>, window: &mut Window, cx: &mut Context<Self>) {
        self.running = true;
        self.error = None;
        cx.notify();
        let script = ddl::script(self.shared.cfg.driver, &statements);
        let conn = self.shared.conn.clone();
        let task = runtime::spawn(async move {
            conn.read()
                .await
                .execute_cancellable(&script, &Cancel::default())
                .await
        });
        let on_done = self.on_done.clone();
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            this.update_in(cx, |editor, window, cx| {
                editor.running = false;
                match result {
                    Ok(_) => {
                        window.push_notification(Notification::success("Structure updated"), cx);
                        on_done(window, cx);
                    }
                    Err(error) => editor.error = Some(format!("{error:#}")),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    #[cfg(test)]
    pub fn set_row_for_test(
        &mut self,
        index: usize,
        name: &str,
        data_type: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        while self.rows.len() <= index {
            self.push_row(None, window, cx);
        }
        let row = &self.rows[index];
        let (name_input, type_input) = (row.name.clone(), row.data_type.clone());
        name_input.update(cx, |input, cx| input.set_value(name, window, cx));
        type_input.update(cx, |input, cx| input.set_value(data_type, window, cx));
    }

    #[cfg(test)]
    pub fn show_ddl_for_test(&mut self, cx: &mut Context<Self>) {
        self.show_ddl = true;
        cx.notify();
    }

    #[cfg(test)]
    pub fn remove_row_for_test(&mut self, index: usize) {
        self.rows[index].removed = true;
    }

    #[cfg(test)]
    pub fn apply_for_test(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.apply(window, cx);
    }

    #[cfg(test)]
    pub fn discard_for_test(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.discard(window, cx);
    }

    fn render_row(&self, index: usize, row: &ColumnRow, cx: &mut Context<Self>) -> AnyElement {
        let t = tokens(cx);
        let driver = self.shared.cfg.driver;
        let state = self.state(row, cx);
        let now = self.definition(row, cx);
        // Clicks find their row by this id: positions shift when a new
        // row is removed before the next redraw.
        let key = row.name.entity_id();
        let tint = match state {
            RowState::Unchanged => None,
            RowState::Altered => Some(t.warning),
            RowState::New => Some(t.success),
            RowState::Removed => Some(t.danger),
        };
        // "was …" under a field the edit changed.
        let was = |changed: bool, before: String| {
            div()
                .h(dp(14.))
                .text_size(dp(10.5))
                .text_color(t.text_muted)
                .when(changed, |hint| hint.child(format!("was {before}")))
        };
        let original = row.original.clone();
        let altered = state == RowState::Altered;
        let changed = |field: fn(&ColDef, &ColDef) -> bool| {
            altered && original.as_ref().is_some_and(|before| field(before, &now))
        };
        let shows_was = altered;
        let data_type = row.data_type.clone();
        h_flex()
            .id(("structure-row", index))
            .items_start()
            .gap(dp(8.))
            .px(dp(12.))
            .pt(dp(6.))
            .border_l_2()
            .border_color(tint.unwrap_or(transparent_black()))
            .when_some(tint, |line, tint| line.bg(tint.opacity(0.07)))
            .when(state == RowState::Removed, |line| line.opacity(0.5))
            .child(
                v_flex()
                    .w(dp(200.))
                    .gap(dp(2.))
                    .child(
                        h_flex()
                            .gap(dp(4.))
                            // Every row keeps the slot so the inputs line up.
                            .child(div().w(dp(14.)).when(now.pk, |slot| {
                                slot.child(
                                    Icon::new(IconName::KeyRound).xsmall().text_color(t.warning),
                                )
                            }))
                            .child(div().flex_1().child(Input::new(&row.name).small())),
                    )
                    .when(shows_was, |field| {
                        field.child(was(
                            changed(|before, now| before.name != now.name),
                            original
                                .as_ref()
                                .map(|o| o.name.clone())
                                .unwrap_or_default(),
                        ))
                    }),
            )
            .child(
                v_flex()
                    .w(dp(220.))
                    .gap(dp(2.))
                    .child(
                        h_flex()
                            .gap(dp(2.))
                            .child(div().flex_1().child(Input::new(&row.data_type).small()))
                            .child(
                                Button::new(("structure-types", index))
                                    .cursor_pointer()
                                    .icon(IconName::ChevronDown)
                                    .ghost()
                                    .xsmall()
                                    .dropdown_menu(move |menu, _, _| {
                                        common_types(driver).iter().fold(menu, |menu, name| {
                                            let data_type = data_type.clone();
                                            menu.item(PopupMenuItem::new(*name).on_click(
                                                move |_, window, cx| {
                                                    data_type.update(cx, |input, cx| {
                                                        input.set_value(*name, window, cx)
                                                    })
                                                },
                                            ))
                                        })
                                    }),
                            ),
                    )
                    .when(shows_was, |field| {
                        field.child(was(
                            changed(|before, now| !before.ty.eq_ignore_ascii_case(&now.ty)),
                            original.as_ref().map(|o| o.ty.clone()).unwrap_or_default(),
                        ))
                    }),
            )
            .child(
                v_flex()
                    .w(dp(60.))
                    .gap(dp(2.))
                    .pt(dp(4.))
                    .child(
                        Checkbox::new(("structure-null", index))
                            .cursor_pointer()
                            .checked(row.nullable)
                            .on_click(cx.listener(move |editor, checked: &bool, _, cx| {
                                if let Some(row) = editor.row_mut(key) {
                                    row.nullable = *checked;
                                }
                                cx.notify();
                            })),
                    )
                    .when(shows_was, |field| {
                        field.child(was(
                            changed(|before, now| before.nullable != now.nullable),
                            if now.nullable { "NOT NULL" } else { "NULL" }.to_string(),
                        ))
                    }),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap(dp(2.))
                    .child(
                        div()
                            .text_color(default_colour(now.default.as_deref(), &t))
                            .child(Input::new(&row.default).small()),
                    )
                    .when(shows_was, |field| {
                        field.child(was(
                            changed(|before, now| before.default != now.default),
                            original
                                .as_ref()
                                .and_then(|o| o.default.clone())
                                .unwrap_or_else(|| "no default".into()),
                        ))
                    }),
            )
            .child(
                div().w(dp(28.)).child(
                    Button::new(("structure-remove", index))
                        .cursor_pointer()
                        .icon(if row.removed {
                            IconName::Undo
                        } else {
                            IconName::Trash
                        })
                        .ghost()
                        .xsmall()
                        .tooltip(if row.removed {
                            "Keep column"
                        } else {
                            "Remove column"
                        })
                        .on_click(cx.listener(move |editor, _, _, cx| {
                            let Some(index) = editor
                                .rows
                                .iter()
                                .position(|row| row.name.entity_id() == key)
                            else {
                                return;
                            };
                            if editor.rows[index].original.is_none() {
                                editor.rows.remove(index);
                            } else {
                                editor.rows[index].removed = !editor.rows[index].removed;
                            }
                            cx.notify();
                        })),
                ),
            )
            .into_any_element()
    }

    /// Preview DDL / Discard / Apply, while anything is staged.
    fn render_bar(&self, count: usize, cx: &mut Context<Self>) -> AnyElement {
        let t = tokens(cx);
        h_flex()
            .flex_none()
            .h(dp(70.))
            .justify_center()
            .child(
                h_flex()
                    .h(dp(46.))
                    .pl(dp(16.))
                    .pr(dp(6.))
                    .gap(dp(10.))
                    .rounded(dp(12.))
                    .bg(t.raised)
                    .border_1()
                    .border_color(t.border)
                    .shadow_lg()
                    .text_size(dp(13.))
                    .child(div().size(dp(8.)).rounded_full().bg(t.warning))
                    .child(
                        div()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(if count == 1 {
                                "1 schema change".to_string()
                            } else {
                                format!("{count} schema changes")
                            }),
                    )
                    .child(div().w(dp(8.)))
                    .child(
                        Button::new("structure-preview")
                            .cursor_pointer()
                            .label(if self.show_ddl {
                                "Hide DDL"
                            } else {
                                "Preview DDL"
                            })
                            .ghost()
                            .small()
                            .on_click(cx.listener(|editor, _, _, cx| {
                                editor.show_ddl = !editor.show_ddl;
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("structure-discard")
                            .cursor_pointer()
                            .label("Discard")
                            .ghost()
                            .small()
                            .on_click(
                                cx.listener(|editor, _, window, cx| editor.discard(window, cx)),
                            ),
                    )
                    .child(
                        Button::new("structure-apply")
                            .cursor_pointer()
                            .label("Apply")
                            .primary()
                            .small()
                            .loading(self.running)
                            .on_click(
                                cx.listener(|editor, _, window, cx| editor.apply(window, cx)),
                            ),
                    ),
            )
            .into_any_element()
    }
}

/// Defaults read like code: functions in the keyword colour, literals in
/// the success green.
fn default_colour(default: Option<&str>, t: &Tokens) -> Hsla {
    match default {
        Some(expression) if expression.contains('(') => t.keyword,
        Some(_) => t.success,
        None => t.text,
    }
}

impl Render for StructureEditor {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = tokens(cx);
        let mono = cx.theme().mono_font_family.clone();
        let count = self.change_count(cx);
        let header = |label: &'static str| {
            div()
                .text_size(dp(11.))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(t.text_muted)
                .child(label)
        };
        let ddl_text = match self.statements(cx) {
            Ok(statements) if statements.is_empty() => "-- no changes".to_string(),
            Ok(statements) => statements.join("\n"),
            Err(error) => format!("-- {error:#}"),
        };
        let rows: Vec<AnyElement> = self
            .rows
            .iter()
            .enumerate()
            .map(|(index, row)| self.render_row(index, row, cx))
            .collect();
        v_flex()
            .size_full()
            .child(
                h_flex()
                    .gap(dp(8.))
                    .px(dp(14.))
                    .py(dp(8.))
                    .border_b_1()
                    .border_color(t.border)
                    .child(div().w(dp(200.)).child(header("COLUMN")))
                    .child(div().w(dp(220.)).child(header("TYPE")))
                    .child(div().w(dp(60.)).child(header("NULL")))
                    .child(div().flex_1().child(header("DEFAULT")))
                    .child(div().w(dp(28.))),
            )
            .child(
                div()
                    .id("structure-rows")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(v_flex().pb(dp(6.)).children(rows))
                    .child(
                        h_flex().px(dp(8.)).py(dp(6.)).child(
                            Button::new("structure-add")
                                .cursor_pointer()
                                .icon(IconName::Plus)
                                .label("Column")
                                .ghost()
                                .small()
                                .on_click(cx.listener(|editor, _, window, cx| {
                                    editor.push_row(None, window, cx);
                                    cx.notify();
                                })),
                        ),
                    )
                    .when(self.shared.cfg.driver == Driver::Sqlite, |list| {
                        list.child(
                            div()
                                .px(dp(14.))
                                .text_size(dp(12.))
                                .text_color(t.text_muted)
                                .child(
                                    "SQLite can add, rename and remove columns; other changes \
                                     need a new table.",
                                ),
                        )
                    }),
            )
            .when_some(self.error.clone(), |column, error| {
                column.child(
                    div()
                        .px(dp(14.))
                        .py(dp(8.))
                        .text_size(dp(12.))
                        .text_color(t.danger)
                        .bg(t.danger.opacity(0.08))
                        .child(error),
                )
            })
            .when(self.show_ddl && count > 0, |column| {
                column.child(
                    div()
                        .id("structure-ddl")
                        .flex_none()
                        .max_h(dp(160.))
                        .overflow_y_scroll()
                        .mx(dp(14.))
                        .p(dp(12.))
                        .rounded(dp(8.))
                        .border_1()
                        .border_color(t.border)
                        .bg(t.surface)
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
            })
            .when(count > 0, |column| column.child(self.render_bar(count, cx)))
    }
}
