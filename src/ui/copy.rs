//! "Copy as …" menu items and "Export…" for grids (table tabs and query
//! results), built on `crate::export`.

use std::cell::Cell;
use std::fs::{File, remove_file, rename};
use std::io::{BufWriter, Write as _};
use std::path::PathBuf;
use std::pin::pin;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use gpui::{
    AnyElement, App, AppContext as _, ClipboardItem, Context, Entity, FocusHandle, Focusable,
    FontWeight, IntoElement, ParentElement as _, Render, SharedString, Styled as _, WeakEntity,
    Window, div, prelude::*,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::checkbox::Checkbox;
use gpui_component::input::{Input, InputState};
use gpui_component::menu::{PopupMenu, PopupMenuItem};
use gpui_component::notification::Notification;
use gpui_component::table::TableState;
use gpui_component::{ActiveTheme as _, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit_assets::IconName;

use super::grid::Grid;
use super::theme::{dp, tokens};
use super::{RunQuery, dialog};
use crate::db::{Connection, TableQuery};
use crate::export::{self, Format, Row, SqlTarget};
use crate::runtime;
use anyhow::{Result, bail};
use dirs::{download_dir, home_dir};
use futures::{future::Either, future::select};
use gpui::relative;
use serde_json::{Value, from_str, to_string_pretty};
use tokio::sync::RwLock;
use uuid::Uuid;

/// Where an export's rows come from.
pub enum Source {
    /// What the grid holds (query results).
    Loaded,
    /// Every row of a table (all pages), with the tab's filters and sort.
    Table {
        conn: Arc<RwLock<Connection>>,
        query: TableQuery,
    },
}

pub trait ExportHost: 'static {
    fn export_grid(&self) -> &Entity<TableState<Grid>>;
    /// The table that SQL output writes into.
    fn sql_target(&self, cx: &App) -> SqlTarget;
    /// File name without extension.
    fn export_name(&self) -> String;
    fn export_source(&self, cx: &App) -> Source;
    /// Rows `export_source` holds, when known (a table's filtered count).
    fn export_total(&self, _cx: &App) -> Option<u64> {
        None
    }
}

fn copy(text: String, what: &str, window: &mut Window, cx: &mut App) {
    cx.write_to_clipboard(ClipboardItem::new_string(text));
    window.push_notification(format!("Copied {what}"), cx);
}

type Action<H> = Rc<dyn Fn(&H, &mut Window, &mut App)>;

/// Copy items for the right-clicked `row` (and the selected column).
/// Row `row` as one-line `format` text (no header line).
fn row_as<H: ExportHost>(host: &H, format: Format, row: usize, cx: &App) -> String {
    let grid = host.export_grid().read(cx).delegate();
    let target = host.sql_target(cx);
    let options = export::Options {
        header: false,
        pretty: true,
    };
    let mut encoder = export::Encoder::new(format, &grid.columns, &target).options(options);
    let mut text = encoder.header();
    text.push_str(&encoder.rows(&[grid.row_values(row)]));
    text.push_str(&encoder.footer());
    text.trim_end().to_string()
}

/// "Copy as ›" (row `row`) and "Copy all as ›" submenus, then Export…
/// (design.md screen 09); shared by table tabs and query results.
pub fn copy_items<H: ExportHost>(
    menu: PopupMenu,
    host: WeakEntity<H>,
    row: usize,
    window: &mut Window,
    cx: &mut App,
) -> PopupMenu {
    let item = |label: &str, action: Action<H>| {
        let host = host.clone();
        PopupMenuItem::new(label.to_string()).on_click(move |_, window, cx| {
            if let Some(host) = host.upgrade() {
                host.update(cx, |host, cx| action(host, window, cx));
            }
        })
    };
    let row_menu = PopupMenu::build(window, cx, |menu, _, _| {
        menu.item(item(
            "JSON",
            Rc::new(move |host: &H, window, cx| {
                let text = export::render(
                    Format::Json,
                    &host.export_grid().read(cx).delegate().columns,
                    &[host.export_grid().read(cx).delegate().row_values(row)],
                    &host.sql_target(cx),
                );
                // A single object, not a one-element array.
                let value: Value = from_str(&text).unwrap_or_default();
                copy(
                    to_string_pretty(&value[0]).unwrap_or_default(),
                    "row as JSON",
                    window,
                    cx,
                )
            }),
        ))
        .item(item(
            "INSERT",
            Rc::new(move |host: &H, window, cx| {
                let grid = host.export_grid().read(cx).delegate();
                let text =
                    export::inserts(&host.sql_target(cx), &grid.columns, &[grid.row_values(row)]);
                copy(text, "row as INSERT", window, cx)
            }),
        ))
        .item(item(
            "UPDATE",
            Rc::new(move |host: &H, window, cx| {
                let grid = host.export_grid().read(cx).delegate();
                let text = export::updates(
                    &host.sql_target(cx),
                    &grid.columns,
                    &grid.pk,
                    &[grid.row_values(row)],
                );
                copy(text, "row as UPDATE", window, cx)
            }),
        ))
        .item(item(
            "CSV",
            Rc::new(move |host: &H, window, cx| {
                copy(row_as(host, Format::Csv, row, cx), "row as CSV", window, cx)
            }),
        ))
        .item(item(
            "TSV",
            Rc::new(move |host: &H, window, cx| {
                copy(row_as(host, Format::Tsv, row, cx), "row as TSV", window, cx)
            }),
        ))
        .item(item(
            "Markdown",
            Rc::new(move |host: &H, window, cx| {
                copy(
                    row_as(host, Format::Markdown, row, cx),
                    "row as Markdown",
                    window,
                    cx,
                )
            }),
        ))
        .separator()
        .item(item(
            "Column values as IN (…)",
            Rc::new(move |host: &H, window, cx| {
                let table = host.export_grid().read(cx);
                let column = table.selected_cell().map(|(_, c)| c).unwrap_or(0);
                let text = export::in_list(
                    host.sql_target(cx).driver,
                    &table.delegate().column_values(column),
                );
                copy(text, "column values", window, cx)
            }),
        ))
        .item(item(
            "Column names",
            Rc::new(move |host: &H, window, cx| {
                let names = host.export_grid().read(cx).delegate().columns.join(", ");
                copy(names, "column names", window, cx)
            }),
        ))
    });
    let all_menu = PopupMenu::build(window, cx, |menu, _, _| {
        [
            ("CSV", Format::Csv),
            ("TSV", Format::Tsv),
            ("JSON", Format::Json),
            ("Markdown", Format::Markdown),
            ("INSERT", Format::SqlInsert),
        ]
        .into_iter()
        .fold(menu, |menu, (label, format)| {
            menu.item(item(
                label,
                Rc::new(move |host: &H, window, cx| copy_all(host, format, window, cx)),
            ))
        })
    });
    menu.item(PopupMenuItem::submenu("Copy as", row_menu))
        .item(PopupMenuItem::submenu("Copy all rows as", all_menu))
        .item(item(
            "Export…",
            Rc::new(|host: &H, window, cx| open_export(host, window, cx)),
        ))
}

fn copy_all<H: ExportHost>(h: &H, f: Format, window: &mut Window, cx: &mut App) {
    let g = h.export_grid().read(cx).delegate();
    let text = export::render(f, &g.columns, &g.all_values(), &h.sql_target(cx));
    copy(text, &format!("as {}", f.label()), window, cx);
}

/// Which rows an export writes (design.md, screen 12).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Scope {
    /// The table with the tab's filters (all pages).
    Filtered,
    /// The table without filters.
    Whole,
    /// What the grid holds: this page, or all query results.
    Loaded,
    /// The selected row.
    Selected,
}

struct ScopeChoice {
    scope: Scope,
    label: &'static str,
    rows: Option<u64>,
}

/// What a running export reports back, shared with the progress toast.
#[derive(Default)]
pub struct Progress {
    done: AtomicUsize,
    cancel: AtomicBool,
}

/// Opens the export dialog for `h`'s grid.
pub fn open_export<H: ExportHost>(h: &H, window: &mut Window, cx: &mut App) {
    let view = cx.new(|cx| ExportDialog::new(h, window, cx));
    let w = dialog::width(640., cx);
    let first = Cell::new(true);
    window.open_dialog(cx, move |d, window, cx| {
        if first.replace(false) {
            view.read(cx).focus.clone().focus(window, cx);
        }
        dialog::shell(d, w, cx).child(view.clone())
    });
}

pub struct ExportDialog {
    name: String,
    columns: Vec<String>,
    loaded: Vec<Row>,
    selected: Option<Row>,
    target: SqlTarget,
    table: Option<(Arc<RwLock<Connection>>, TableQuery)>,
    scopes: Vec<ScopeChoice>,
    scope: usize,
    format: Format,
    options: export::Options,
    path: Entity<InputState>,
    focus: FocusHandle,
}

impl ExportDialog {
    fn new<H: ExportHost>(h: &H, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let state = h.export_grid().read(cx);
        let g = state.delegate();
        let selected = state
            .selected_row()
            .filter(|r| *r < g.rows.len() && !g.deleted.contains(r))
            .map(|r| g.row_values(r));
        let (columns, loaded) = (g.columns.clone(), g.all_values());
        let target = h.sql_target(cx);
        let name = h.export_name();
        let total = h.export_total(cx);
        let table = match h.export_source(cx) {
            Source::Table { conn, query } => Some((conn, query)),
            Source::Loaded => None,
        };
        let n = loaded.len() as u64;
        let mut scopes = vec![];
        match &table {
            Some((_, q)) => {
                if q.filters.is_empty() {
                    scopes.push(ScopeChoice {
                        scope: Scope::Whole,
                        label: "Whole table",
                        rows: total,
                    });
                } else {
                    scopes.push(ScopeChoice {
                        scope: Scope::Filtered,
                        label: "Current filter",
                        rows: total,
                    });
                    scopes.push(ScopeChoice {
                        scope: Scope::Whole,
                        label: "Whole table",
                        rows: None,
                    });
                }
                if total.is_none_or(|t| t > n) {
                    scopes.push(ScopeChoice {
                        scope: Scope::Loaded,
                        label: "This page",
                        rows: Some(n),
                    });
                }
            }
            None => scopes.push(ScopeChoice {
                scope: Scope::Loaded,
                label: "All results",
                rows: Some(n),
            }),
        }
        if selected.is_some() {
            scopes.push(ScopeChoice {
                scope: Scope::Selected,
                label: "Selected row",
                rows: Some(1),
            });
        }
        let format = Format::Csv;
        let dir = download_dir()
            .or_else(home_dir)
            .unwrap_or_else(|| PathBuf::from("."));
        let file = dir.join(format!("{name}.{}", format.extension()));
        let path = cx.new(|cx| {
            InputState::new(window, cx).default_value(file.to_string_lossy().to_string())
        });
        Self {
            name,
            columns,
            loaded,
            selected,
            target,
            table,
            scopes,
            scope: 0,
            format,
            options: export::Options::default(),
            path,
            focus: cx.focus_handle(),
        }
    }

    fn set_format(&mut self, format: Format, window: &mut Window, cx: &mut Context<Self>) {
        let old = self.format;
        self.format = format;
        // Swap the extension only if it's still the one we chose.
        let path = self.path.read(cx).value().to_string();
        let suffix = format!(".{}", old.extension());
        if let Some(stem) = path.strip_suffix(&suffix) {
            let new = format!("{stem}.{}", format.extension());
            self.path.update(cx, |s, cx| s.set_value(new, window, cx));
        }
        cx.notify();
    }

    fn choose_path(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let current = expand_home(self.path.read(cx).value().trim());
        let dir = current
            .parent()
            .filter(|d| d.is_dir())
            .map(PathBuf::from)
            .or_else(download_dir)
            .or_else(home_dir)
            .unwrap_or_else(|| PathBuf::from("."));
        let file = current
            .file_name()
            .map(|f| f.to_string_lossy().to_string())
            .unwrap_or_else(|| format!("{}.{}", self.name, self.format.extension()));
        let picked = cx.prompt_for_new_path(&dir, Some(&file));
        let input = self.path.clone();
        cx.spawn_in(window, async move |_, cx| {
            if let Ok(Ok(Some(p))) = picked.await {
                cx.update(|window, cx| {
                    input.update(cx, |s, cx| {
                        s.set_value(p.to_string_lossy().to_string(), window, cx)
                    })
                })
                .ok();
            }
        })
        .detach();
    }

    /// The rows the preview shows: the first two of what's loaded (or the
    /// selected row).
    fn preview(&self) -> String {
        let rows: Vec<Row> = match self.scopes[self.scope].scope {
            Scope::Selected => self.selected.iter().cloned().collect(),
            _ => self.loaded.iter().take(2).cloned().collect(),
        };
        let mut e =
            export::Encoder::new(self.format, &self.columns, &self.target).options(self.options);
        let mut out = e.header();
        out.push_str(&e.rows(&rows));
        out.push_str(&e.footer());
        if self.format == Format::Json || self.format == Format::Html {
            out = out.lines().take(14).collect::<Vec<_>>().join("\n");
        }
        out
    }

    fn export(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let raw = self.path.read(cx).value().trim().to_string();
        let path = expand_home(&raw);
        if raw.is_empty() || path.file_name().is_none() {
            window.push_notification(Notification::error("Choose where to save the file"), cx);
            return;
        }
        if !path
            .parent()
            .is_some_and(|d| d.as_os_str().is_empty() || d.is_dir())
        {
            window.push_notification(
                Notification::error(format!("Folder not found: {}", path.display())),
                cx,
            );
            return;
        }
        let choice = &self.scopes[self.scope];
        let (source, rows) = match (choice.scope, &self.table) {
            (Scope::Filtered, Some((conn, q))) => (
                Source::Table {
                    conn: conn.clone(),
                    query: q.clone(),
                },
                vec![],
            ),
            (Scope::Whole, Some((conn, q))) => {
                let mut q = q.clone();
                q.filters.clear();
                (
                    Source::Table {
                        conn: conn.clone(),
                        query: q,
                    },
                    vec![],
                )
            }
            (Scope::Selected, _) => (Source::Loaded, self.selected.iter().cloned().collect()),
            _ => (Source::Loaded, self.loaded.clone()),
        };
        let job = Job {
            format: self.format,
            options: self.options,
            columns: self.columns.clone(),
            rows,
            target: self.target.clone(),
            source,
            path,
        };
        window.close_dialog(cx);
        run_export(job, choice.rows, window, cx);
    }
}

/// "1 row", "12,345 rows".
fn rows_label(n: u64) -> String {
    if n == 1 {
        "1 row".into()
    } else {
        format!("{} rows", group(n))
    }
}

/// 12345 → "12,345".
fn group(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, ch) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

fn expand_home(p: &str) -> PathBuf {
    match (p.strip_prefix("~/"), home_dir()) {
        (Some(rest), Some(home)) => home.join(rest),
        _ => PathBuf::from(p),
    }
}

impl Focusable for ExportDialog {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for ExportDialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = tokens(cx);
        let mono = cx.theme().mono_font_family.clone();
        let entity = cx.entity();
        let sub = match self.scopes[0].rows {
            Some(n) => format!("{} · {}", self.name, rows_label(n)),
            None => self.name.clone(),
        };
        let tiles: Vec<AnyElement> = Format::ALL
            .iter()
            .map(|&f| {
                let entity = entity.clone();
                dialog::tile(
                    SharedString::from(format!("export-format-{}", f.extension())),
                    Some(format_icon(f)),
                    f.short_label(),
                    format!(".{}", f.extension()),
                    f == self.format,
                    cx,
                )
                .on_click(move |_, window, cx| {
                    entity.update(cx, |this, cx| this.set_format(f, window, cx))
                })
                .into_any_element()
            })
            .collect();
        let scopes: Vec<AnyElement> = self
            .scopes
            .iter()
            .enumerate()
            .map(|(i, s)| {
                let entity = entity.clone();
                dialog::choice(
                    SharedString::from(format!("export-scope-{i}")),
                    s.label,
                    Some(
                        s.rows
                            .map(rows_label)
                            .unwrap_or_else(|| "all rows".into())
                            .into(),
                    ),
                    i == self.scope,
                    cx,
                )
                .on_click(move |_, _, cx| {
                    entity.update(cx, |this, cx| {
                        this.scope = i;
                        cx.notify();
                    })
                })
                .into_any_element()
            })
            .collect();
        let options = self.options_row(cx);
        v_flex()
            .track_focus(&self.focus)
            .key_context("ExportDialog")
            .gap(dp(18.))
            .on_action(cx.listener(|this, _: &RunQuery, window, cx| this.export(window, cx)))
            .child(dialog::title("Export", Some(sub.into()), cx))
            .child(dialog::section(
                "Format",
                div().grid().grid_cols(4).gap(dp(8.)).children(tiles),
                cx,
            ))
            .child(
                h_flex()
                    .items_start()
                    .gap(dp(20.))
                    .child(
                        dialog::section("Rows", v_flex().gap(dp(2.)).children(scopes), cx).flex_1(),
                    )
                    .child(dialog::section("Options", options, cx).flex_1()),
            )
            .child(dialog::section(
                "Preview",
                div()
                    .id("export-preview")
                    .max_h(dp(150.))
                    .overflow_y_scroll()
                    .p(dp(12.))
                    .rounded(dp(8.))
                    .border_1()
                    .border_color(t.border)
                    .bg(t.canvas)
                    .font_family(mono)
                    .text_size(dp(11.5))
                    .text_color(t.text_secondary)
                    .whitespace_nowrap()
                    .overflow_x_hidden()
                    .children(self.preview().lines().map(|l| div().child(l.to_string()))),
                cx,
            ))
            .child(dialog::section(
                "Destination",
                h_flex()
                    .gap(dp(8.))
                    .child(
                        div()
                            .flex_1()
                            .debug_selector(|| "export-path".into())
                            .child(Input::new(&self.path)),
                    )
                    .child(
                        Button::new("export-choose")
                            .cursor_pointer()
                            .label("Choose…")
                            .outline()
                            .on_click(
                                cx.listener(|this, _, window, cx| this.choose_path(window, cx)),
                            ),
                    ),
                cx,
            ))
            .child(dialog::footer(
                "⌘↵ export · Esc cancels",
                vec![
                    Button::new("export-cancel")
                        .cursor_pointer()
                        .label("Cancel")
                        .outline()
                        .on_click(|_, window, cx| window.close_dialog(cx))
                        .into_any_element(),
                    Button::new("export-ok")
                        .cursor_pointer()
                        .label("Export")
                        .primary()
                        .on_click(cx.listener(|this, _, window, cx| this.export(window, cx)))
                        .into_any_element(),
                ],
                cx,
            ))
    }
}

impl ExportDialog {
    /// The options that apply to the chosen format.
    fn options_row(&self, cx: &mut Context<Self>) -> AnyElement {
        let t = tokens(cx);
        let o = self.options;
        let note = |s: String| {
            div()
                .text_size(dp(12.))
                .text_color(t.text_muted)
                .child(s)
                .into_any_element()
        };
        match self.format {
            Format::Csv | Format::Tsv | Format::Markdown => v_flex()
                .gap(dp(8.))
                .child(
                    div().debug_selector(|| "export-header".into()).child(
                        Checkbox::new("export-header")
                            .cursor_pointer()
                            .checked(o.header)
                            .label("Column names as the first row")
                            .on_click(cx.listener(|this, v: &bool, _, cx| {
                                this.options.header = *v;
                                cx.notify();
                            })),
                    ),
                )
                .when(self.format != Format::Markdown, |d| {
                    d.child(note(
                        "NULL is an empty field; an empty string is \"\".".into(),
                    ))
                })
                .into_any_element(),
            Format::Json => Checkbox::new("export-pretty")
                .cursor_pointer()
                .checked(o.pretty)
                .label("Pretty-print (one indented object per row)")
                .on_click(cx.listener(|this, v: &bool, _, cx| {
                    this.options.pretty = *v;
                    cx.notify();
                }))
                .into_any_element(),
            Format::JsonLines => note("One JSON object per line.".into()),
            Format::Html => note("A standalone page with one table.".into()),
            Format::SqlInsert => note(format!(
                "One INSERT per row into {}.",
                self.target.qualified()
            )),
        }
    }
}

fn format_icon(f: Format) -> IconName {
    match f {
        Format::Csv | Format::Tsv => IconName::Sheet,
        Format::Json | Format::JsonLines => IconName::Braces,
        Format::Markdown => IconName::FileText,
        Format::Html => IconName::Code,
        Format::SqlInsert => IconName::Database,
    }
}

/// Everything a running export needs.
struct Job {
    format: Format,
    options: export::Options,
    columns: Vec<String>,
    /// For `Source::Loaded`.
    rows: Vec<Row>,
    target: SqlTarget,
    source: Source,
    path: PathBuf,
}

/// The progress toast's key: one per export.
struct ExportToast;

/// Writes `job` in the background. A long export shows a toast with rows
/// done, an estimate and Cancel; the end is always a notification.
fn run_export(job: Job, total: Option<u64>, window: &mut Window, cx: &mut App) {
    let progress = Arc::new(Progress::default());
    let key = SharedString::from(Uuid::new_v4().to_string());
    let path = job.path.clone();
    let task = write_export(job, progress.clone());
    let toast = cx.new(|_| ExportProgress {
        progress: progress.clone(),
        total,
        started: Instant::now(),
    });
    window
        .spawn(cx, async move |cx| {
            let res = {
                let mut task = pin!(task);
                let delay = cx.background_executor().timer(Duration::from_millis(700));
                match select(task.as_mut(), delay).await {
                    Either::Left((res, _)) => res,
                    Either::Right(_) => {
                        let (shown, key) = (toast.clone(), key.clone());
                        cx.update(|window, cx| {
                            ExportProgress::show(shown, key, window, cx);
                        })
                        .ok();
                        // Repaint the toast until the export ends.
                        let ticker = {
                            let toast = toast.clone();
                            let mut cx = cx.clone();
                            async move {
                                loop {
                                    cx.background_executor()
                                        .timer(Duration::from_millis(250))
                                        .await;
                                    if cx
                                        .update(|_, cx| toast.update(cx, |_, cx| cx.notify()))
                                        .is_err()
                                    {
                                        break;
                                    }
                                }
                            }
                        };
                        let ticker = pin!(ticker);
                        match select(task, ticker).await {
                            Either::Left((res, _)) => res,
                            // The window went away; the file still gets written.
                            Either::Right((_, task)) => task.await,
                        }
                    }
                }
            };
            cx.update(|window, cx| {
                window.remove_notification1::<ExportToast>(key, cx);
                match res {
                    Ok(n) => {
                        let shown = path.display().to_string();
                        let reveal = if cfg!(target_os = "macos") {
                            "Show in Finder"
                        } else {
                            "Show in folder"
                        };
                        window.push_notification(
                            Notification::success(format!(
                                "Exported {} to {shown}",
                                rows_label(n as u64)
                            ))
                            .action(move |_, _, _| {
                                let path = path.clone();
                                Button::new("export-reveal")
                                    .cursor_pointer()
                                    .label(reveal)
                                    .outline()
                                    .xsmall()
                                    .on_click(move |_, _, cx| cx.reveal_path(&path))
                            }),
                            cx,
                        );
                    }
                    Err(_) if progress.cancel.load(Ordering::Relaxed) => {
                        window.push_notification("Export cancelled", cx)
                    }
                    Err(e) => window.push_notification(
                        Notification::error(format!("Export failed: {e:#}")),
                        cx,
                    ),
                }
            })
            .ok();
        })
        .detach();
}

/// The toast body of a running export (design.md, Components → Toast).
struct ExportProgress {
    progress: Arc<Progress>,
    total: Option<u64>,
    started: Instant,
}

impl ExportProgress {
    fn show(view: Entity<Self>, key: SharedString, window: &mut Window, cx: &mut App) {
        let note = Notification::new()
            .id1::<ExportToast>(key)
            .autohide(false)
            .content(move |_, _, _| view.clone().into_any_element());
        window.push_notification(note, cx);
    }
}

impl Render for ExportProgress {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = tokens(cx);
        let done = self.progress.done.load(Ordering::Relaxed) as u64;
        let fraction = self
            .total
            .filter(|t| *t > 0)
            .map(|t| (done as f32 / t as f32).min(1.));
        let rows = match self.total {
            Some(total) => format!("{} / {} rows", group(done), group(total)),
            None => format!("{} rows", group(done)),
        };
        let eta = fraction.filter(|f| *f > 0.02).map(|f| {
            let spent = self.started.elapsed().as_secs_f32();
            let left = (spent / f - spent).max(0.).round() as u64;
            if left >= 60 {
                format!("about {} min left", left.div_ceil(60))
            } else {
                format!("about {left} s left")
            }
        });
        let cancelling = self.progress.cancel.load(Ordering::Relaxed);
        let progress = self.progress.clone();
        v_flex()
            .w(dp(300.))
            .gap(dp(8.))
            .child(
                h_flex()
                    .gap(dp(8.))
                    .child(
                        div()
                            .text_size(dp(13.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(if cancelling {
                                "Cancelling…"
                            } else {
                                "Exporting…"
                            }),
                    )
                    .child(div().flex_1())
                    .child(
                        Button::new("export-cancel-run")
                            .cursor_pointer()
                            .label("Cancel")
                            .ghost()
                            .xsmall()
                            .on_click(move |_, _, _| {
                                progress.cancel.store(true, Ordering::Relaxed)
                            }),
                    ),
            )
            .child(
                div().h(dp(6.)).w_full().rounded_full().bg(t.hover).child(
                    div()
                        .h_full()
                        .rounded_full()
                        .bg(t.accent)
                        .w(relative(fraction.unwrap_or(0.))),
                ),
            )
            .child(
                h_flex()
                    .text_size(dp(11.5))
                    .text_color(t.text_muted)
                    .child(rows)
                    .child(div().flex_1())
                    .children(eta),
            )
    }
}

/// Rows per page when exporting a whole table.
const PAGE: usize = 5000;

async fn write_export(job: Job, progress: Arc<Progress>) -> Result<usize> {
    let Job {
        format,
        options,
        columns,
        rows,
        target,
        source,
        path,
    } = job;
    // Written next to the destination, then renamed: a failed or cancelled
    // export never leaves a half file under the chosen name.
    let tmp = path.with_extension(format!("tabula-{}.tmp", Uuid::new_v4().simple()));
    let result = runtime::spawn({
        let tmp = tmp.clone();
        async move {
            let mut out = BufWriter::new(File::create(&tmp)?);
            match source {
                Source::Loaded => {
                    let mut e = export::Encoder::new(format, &columns, &target).options(options);
                    out.write_all(e.header().as_bytes())?;
                    out.write_all(e.rows(&rows).as_bytes())?;
                    out.write_all(e.footer().as_bytes())?;
                    out.flush()?;
                    progress.done.store(rows.len(), Ordering::Relaxed);
                    Ok(rows.len())
                }
                // Page by page, so a big table is never all in memory.
                Source::Table { conn, query } => {
                    let c = conn.read().await;
                    let mut q = query;
                    q.limit = PAGE;
                    q.offset = 0;
                    let (first, _) = c.fetch_table(&q).await?;
                    let columns = first.columns.clone();
                    let mut e = export::Encoder::new(format, &columns, &target).options(options);
                    out.write_all(e.header().as_bytes())?;
                    let mut page = first.rows;
                    let mut n = 0;
                    loop {
                        if progress.cancel.load(Ordering::Relaxed) {
                            bail!("cancelled");
                        }
                        n += page.len();
                        out.write_all(e.rows(&page).as_bytes())?;
                        progress.done.store(n, Ordering::Relaxed);
                        if page.len() < PAGE {
                            break;
                        }
                        q.offset += PAGE;
                        page = c.fetch_table(&q).await?.0.rows;
                    }
                    out.write_all(e.footer().as_bytes())?;
                    out.flush()?;
                    Ok(n)
                }
            }
        }
    })
    .await;
    match result {
        Ok(n) => {
            rename(&tmp, &path)?;
            Ok(n)
        }
        Err(e) => {
            let _ = remove_file(&tmp);
            Err(e)
        }
    }
}

/// For ⌘C with a whole row selected: the row's values, tab-separated.
pub fn row_tsv(g: &Grid, r: usize) -> String {
    g.row_values(r)
        .iter()
        .map(|v| v.clone().unwrap_or_default())
        .collect::<Vec<_>>()
        .join("\t")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{ConnectionConfig, DbObject, Driver, ObjectKind};
    use futures::executor::block_on;
    use std::env::temp_dir;
    use std::fs::{read_to_string, write};
    use tokio::sync::RwLock;

    /// A whole-table export reads every page, not just the one on screen.
    #[test]
    fn groups_digits() {
        assert_eq!(group(0), "0");
        assert_eq!(group(999), "999");
        assert_eq!(group(1000), "1,000");
        assert_eq!(group(12345678), "12,345,678");
        assert_eq!(rows_label(1), "1 row");
    }

    #[test]
    fn table_export_reads_all_pages() {
        let mut cfg = ConnectionConfig::new(Driver::Sqlite);
        let db = temp_dir().join(format!("tabula-export-{}.db", cfg.id));
        write(&db, b"").unwrap();
        cfg.file = db.to_string_lossy().into();
        let out = temp_dir().join(format!("tabula-export-{}.csv", cfg.id));
        let out2 = out.clone();
        let n = block_on(runtime::spawn(async move {
            let c = Connection::connect(&cfg).await?;
            c.execute(
                "create table t (id integer primary key, v text); \
                 with recursive n(x) as (select 1 union all select x + 1 from n where x < 12345) \
                 insert into t (v) select 'row ' || x from n",
            )
            .await?;
            let conn = Arc::new(RwLock::new(c));
            let query = TableQuery {
                find: String::new(),
                fields: vec![],
                object: DbObject {
                    schema: None,
                    name: "t".into(),
                    kind: ObjectKind::Table,
                },
                filters: vec![],
                order: Some(("id".into(), true)),
                limit: 300,
                offset: 600,
                match_any: false,
            };
            let target = SqlTarget {
                driver: Driver::Sqlite,
                schema: None,
                table: "t".into(),
            };
            let source = Source::Table { conn, query };
            let job = Job {
                format: Format::Csv,
                options: export::Options::default(),
                columns: vec![],
                rows: vec![],
                target,
                source,
                path: out2,
            };
            write_export(job, Default::default()).await
        }))
        .unwrap();
        assert_eq!(n, 12345);
        let text = read_to_string(&out).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 12346);
        assert_eq!(lines[0], "id,v");
        assert_eq!(lines[1], "1,row 1");
        assert_eq!(lines[12345], "12345,row 12345");
        let _ = remove_file(out);
        let _ = remove_file(db);
    }
}
