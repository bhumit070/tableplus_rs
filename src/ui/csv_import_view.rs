//! Import CSV dialog (design.md screen 13): what the file is (rows,
//! columns, size, encoding, delimiter), the options (header row, empty =
//! NULL, skip rows that fail validation), and the column mapping — each CSV
//! column to a table column or Skip, with three sample values and ✓ / ⚠ / ×.
//! Unmapped required columns block Import; values are checked locally with
//! the cell editors' rules before anything is sent.

use std::collections::HashSet;
use std::iter::once;
use std::rc::Rc;

use gpui::{
    AnyElement, App, AppContext as _, Context, Entity, FontWeight, IntoElement, ParentElement as _,
    Render, SharedString, Styled as _, Subscription, Window, div, prelude::*,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::checkbox::Checkbox;
use gpui_component::select::{Select, SelectEvent, SelectState};
use gpui_component::{
    ActiveTheme as _, Disableable as _, IndexPath, Sizable as _, WindowExt as _, h_flex, v_flex,
};

use super::cell_value::check;
use super::dialog;
use super::theme::{dp, tokens};
use super::{SearchList, search_list};
use crate::csv_import::{
    Csv, Options, changes_with, default_mapping, invalid_rows, missing_required,
};
use crate::db::{Change, ColumnInfo};

/// The mapping list's "leave this CSV column out" entry.
const SKIP: &str = "— skip —";

/// What Import does with the inserts: run them (through the safe-mode
/// gate) against the table.
pub type OnImport = Rc<dyn Fn(Vec<Change>, &mut Window, &mut App)>;

pub struct CsvImportView {
    csv: Csv,
    file_name: String,
    file_size: u64,
    table_title: String,
    columns: Vec<ColumnInfo>,
    options: Options,
    /// Per CSV column: which table column it fills.
    targets: Vec<Entity<SelectState<SearchList>>>,
    on_import: OnImport,
    _subscriptions: Vec<Subscription>,
}

impl CsvImportView {
    #[allow(clippy::too_many_arguments)]
    pub fn open(
        csv: Csv,
        file_name: String,
        file_size: u64,
        table_title: String,
        columns: Vec<ColumnInfo>,
        on_import: OnImport,
        window: &mut Window,
        cx: &mut App,
    ) -> Entity<Self> {
        let view = cx.new(|cx| {
            let mut view = CsvImportView {
                csv,
                file_name,
                file_size,
                table_title,
                columns,
                options: Options::default(),
                targets: vec![],
                on_import,
                _subscriptions: vec![],
            };
            view.rebuild_targets(window, cx);
            view
        });
        let width = dialog::width(860., cx);
        let shown = view.clone();
        window.open_dialog(cx, move |d, _, cx| {
            dialog::shell(d, width, cx).child(shown.clone())
        });
        view
    }

    /// One mapping select per CSV column, starting from the default
    /// mapping; rebuilt when the header option changes the columns.
    fn rebuild_targets(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (headers, _) = self.csv.table(self.options.header);
        let mapping = default_mapping(&headers, &self.columns, self.options.header);
        let choices: Vec<String> = once(SKIP.to_string())
            .chain(self.columns.iter().map(|c| c.name.clone()))
            .collect();
        self._subscriptions.clear();
        self.targets = mapping
            .iter()
            .map(|target| {
                let index = target
                    .as_ref()
                    .and_then(|name| choices.iter().position(|c| c == name))
                    .unwrap_or(0);
                let list = search_list(choices.clone());
                cx.new(|cx| {
                    SelectState::new(list, Some(IndexPath::new(index)), window, cx).searchable(true)
                })
            })
            .collect();
        for target in self.targets.clone() {
            self._subscriptions.push(cx.subscribe_in(
                &target,
                window,
                |_, _, _: &SelectEvent<SearchList>, _, cx| cx.notify(),
            ));
        }
        cx.notify();
    }

    fn mapping(&self, cx: &App) -> Vec<Option<String>> {
        self.targets
            .iter()
            .map(|target| {
                target
                    .read(cx)
                    .selected_value()
                    .filter(|name| name.as_str() != SKIP)
                    .cloned()
            })
            .collect()
    }

    /// Rows whose values the table won't take, by the cell editors' rules.
    fn invalid(&self, cx: &App) -> Vec<(usize, String)> {
        let (_, rows) = self.csv.table(self.options.header);
        invalid_rows(&rows, &self.mapping(cx), &self.columns, |ty, text| {
            check(ty, text).error.is_none()
        })
    }

    fn import(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let mapping = self.mapping(cx);
        let (_, rows) = self.csv.table(self.options.header);
        let invalid = self.invalid(cx);
        if !invalid.is_empty() && !self.options.skip_invalid {
            return;
        }
        let skipped: Vec<usize> = invalid.iter().map(|(row, _)| *row).collect();
        let changes = changes_with(&rows, &mapping, self.options, |row| !skipped.contains(&row));
        window.close_dialog(cx);
        let on_import = self.on_import.clone();
        // After this dialog has gone: the safe-mode confirmation may open.
        window.defer(cx, move |window, cx| on_import(changes, window, cx));
    }

    fn toggle(
        &mut self,
        change: impl FnOnce(&mut Options),
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let header = self.options.header;
        change(&mut self.options);
        if self.options.header != header {
            self.rebuild_targets(window, cx);
        }
        cx.notify();
    }

    #[cfg(test)]
    pub fn import_for_test(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.import(window, cx);
    }

    #[cfg(test)]
    pub fn skip_invalid_for_test(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.toggle(|o| o.skip_invalid = true, window, cx);
    }

    fn render_mapping(&self, cx: &mut Context<Self>) -> AnyElement {
        let t = tokens(cx);
        let mono = cx.theme().mono_font_family.clone();
        let (headers, rows) = self.csv.table(self.options.header);
        let mapping = self.mapping(cx);
        let invalid = self.invalid(cx);
        let header = |label: &'static str, width: Option<f32>| {
            div()
                .when_some(width, |cell, width| cell.w(dp(width)))
                .when(width.is_none(), |cell| cell.flex_1())
                .text_size(dp(11.))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(t.text_muted)
                .child(label)
        };
        v_flex()
            .gap(dp(6.))
            .child(
                h_flex()
                    .gap(dp(10.))
                    .child(header("CSV COLUMN", Some(170.)))
                    .child(header("SAMPLE VALUES", None))
                    .child(header("INTO", Some(220.)))
                    .child(header("", Some(150.))),
            )
            .child(
                div()
                    .id("csv-mapping")
                    .max_h(dp(260.))
                    .overflow_y_scroll()
                    .child(
                        v_flex()
                            .gap(dp(4.))
                            .children(headers.iter().enumerate().map(|(index, name)| {
                                let samples: Vec<String> = rows
                                    .iter()
                                    .filter_map(|row| row.get(index).cloned().flatten())
                                    .take(3)
                                    .collect();
                                let target = mapping.get(index).cloned().flatten();
                                let problems = invalid
                                    .iter()
                                    .filter(|(_, column)| Some(column) == target.as_ref())
                                    .count();
                                let (mark, color, note) = match (&target, problems) {
                                    (None, _) => ("×", t.text_faint, "skipped".to_string()),
                                    (Some(_), 0) => ("✓", t.success, String::new()),
                                    (Some(_), n) => (
                                        "⚠",
                                        t.warning,
                                        if n == 1 {
                                            "1 bad value".into()
                                        } else {
                                            format!("{n} bad values")
                                        },
                                    ),
                                };
                                h_flex()
                                    .gap(dp(10.))
                                    .h(dp(32.))
                                    .child(
                                        div()
                                            .w(dp(170.))
                                            .truncate()
                                            .text_size(dp(12.5))
                                            .text_color(t.text)
                                            .child(name.clone()),
                                    )
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .truncate()
                                            .font_family(mono.clone())
                                            .text_size(dp(11.5))
                                            .text_color(t.text_muted)
                                            .child(samples.join("  ·  ")),
                                    )
                                    .child(div().w(dp(220.)).child(
                                        Select::new(&self.targets[index]).cursor_pointer().small(),
                                    ))
                                    .child(
                                        h_flex()
                                            .w(dp(150.))
                                            .gap(dp(6.))
                                            .text_size(dp(12.))
                                            .text_color(color)
                                            .child(mark)
                                            .child(note),
                                    )
                            })),
                    ),
            )
            .into_any_element()
    }
}

impl Render for CsvImportView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = tokens(cx);
        let (headers, rows) = self.csv.table(self.options.header);
        let mapping = self.mapping(cx);
        let missing = missing_required(&self.columns, &mapping);
        let invalid = self.invalid(cx);
        let mapped = mapping.iter().flatten().count();
        let kept = if self.options.skip_invalid {
            rows.len()
                - invalid
                    .iter()
                    .map(|(row, _)| row)
                    .collect::<HashSet<_>>()
                    .len()
        } else {
            rows.len()
        };
        // Line numbers in the file for the first few problems.
        let first_line = if self.options.header { 2 } else { 1 };
        let problem_lines = invalid
            .iter()
            .take(5)
            .map(|(row, column)| format!("line {} ({column})", row + first_line))
            .collect::<Vec<_>>()
            .join(", ");
        let blocked = mapped == 0
            || !missing.is_empty()
            || (!invalid.is_empty() && !self.options.skip_invalid);
        let delimiter = match self.csv.delimiter {
            '\t' => "tab".to_string(),
            other => format!("“{other}”"),
        };
        let size = if self.file_size >= 1 << 20 {
            format!("{:.1} MB", self.file_size as f64 / (1 << 20) as f64)
        } else {
            format!("{:.0} KB", (self.file_size as f64 / 1024.).max(1.))
        };
        let option = |id: &'static str,
                      label: &'static str,
                      checked: bool,
                      change: fn(&mut Options),
                      cx: &mut Context<Self>| {
            Checkbox::new(id)
                .cursor_pointer()
                .checked(checked)
                .label(label)
                .on_click(
                    cx.listener(move |view, _: &bool, window, cx| view.toggle(change, window, cx)),
                )
        };
        v_flex()
            .gap(dp(16.))
            .child(dialog::title(
                format!("Import CSV into {}", self.table_title),
                Some(
                    format!(
                        "{} · {} rows · {} columns · {size} · {} · {delimiter} separated",
                        self.file_name,
                        rows.len(),
                        headers.len(),
                        self.csv.encoding
                    )
                    .into(),
                ),
                cx,
            ))
            .child(
                h_flex()
                    .gap(dp(20.))
                    .child(option(
                        "csv-header",
                        "First row is the header",
                        self.options.header,
                        |o| o.header = !o.header,
                        cx,
                    ))
                    .child(option(
                        "csv-empty-null",
                        "Empty fields are NULL",
                        self.options.empty_is_null,
                        |o| o.empty_is_null = !o.empty_is_null,
                        cx,
                    ))
                    .child(div().debug_selector(|| "csv-skip-invalid".into()).child(option(
                        "csv-skip-invalid",
                        "Skip rows that fail validation",
                        self.options.skip_invalid,
                        |o| o.skip_invalid = !o.skip_invalid,
                        cx,
                    ))),
            )
            .child(dialog::section("Columns", self.render_mapping(cx), cx))
            .when(!missing.is_empty(), |column| {
                column.child(
                    div().text_size(dp(12.)).text_color(t.danger).child(format!(
                        "Needs a value for: {} (NOT NULL, no default). Map a CSV column to it.",
                        missing.join(", ")
                    )),
                )
            })
            .when(!invalid.is_empty(), |column| {
                column.child(
                    div()
                        .text_size(dp(12.))
                        .text_color(if self.options.skip_invalid { t.text_muted } else { t.warning })
                        .child(if self.options.skip_invalid {
                            format!(
                                "Skipping {} that fail validation: {problem_lines}{}",
                                match rows.len() - kept {
                                    1 => "1 row".to_string(),
                                    n => format!("{n} rows"),
                                },
                                if invalid.len() > 5 { "…" } else { "" }
                            )
                        } else {
                            format!(
                                "{} fit their column: {problem_lines}{}. Fix the file, change the mapping, or skip them.",
                                if invalid.len() == 1 {
                                    "1 row doesn't".to_string()
                                } else {
                                    format!("{} rows don't", invalid.len())
                                },
                                if invalid.len() > 5 { "…" } else { "" }
                            )
                        }),
                )
            })
            .child(dialog::footer(
                "All rows go in one transaction where the database supports it: if one fails, none are inserted.",
                vec![
                    Button::new("csv-cancel")
                        .cursor_pointer()
                        .label("Cancel")
                        .outline()
                        .on_click(|_, window, cx| window.close_dialog(cx))
                        .into_any_element(),
                    Button::new("csv-import")
                        .cursor_pointer()
                        .label(SharedString::from(format!(
                            "Import {kept} row{}",
                            if kept == 1 { "" } else { "s" }
                        )))
                        .primary()
                        .disabled(blocked)
                        .on_click(cx.listener(|view, _, window, cx| view.import(window, cx)))
                        .into_any_element(),
                ],
                cx,
            ))
    }
}
