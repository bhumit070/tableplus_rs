//! "Copy as …" menu items and "Export…" for grids (table tabs and query
//! results), built on `crate::export`.

use std::path::PathBuf;
use std::rc::Rc;

use gpui::{App, ClipboardItem, Entity, WeakEntity, Window};
use gpui_component::WindowExt as _;
use gpui_component::menu::{PopupMenu, PopupMenuItem};
use gpui_component::notification::Notification;
use gpui_component::table::TableState;
use gpui_kit_assets::IconName;

use super::grid::Grid;
use super::picker::{OnPick, PickItem, Picker};
use crate::db::{Connection, TableQuery};
use crate::export::{self, Format, Row, SqlTarget};
use crate::runtime;

/// Where an export's rows come from.
pub enum Source {
    /// What the grid holds (query results).
    Loaded,
    /// Every row of a table (all pages), with the tab's filters and sort.
    Table {
        conn: std::sync::Arc<tokio::sync::RwLock<Connection>>,
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
}

fn copy(text: String, what: &str, window: &mut Window, cx: &mut App) {
    cx.write_to_clipboard(ClipboardItem::new_string(text));
    window.push_notification(format!("Copied {what}"), cx);
}

type Action<H> = Rc<dyn Fn(&H, &mut Window, &mut App)>;

/// Copy items for the right-clicked `row` (and the selected column).
pub fn copy_items<H: ExportHost>(menu: PopupMenu, host: WeakEntity<H>, row: usize) -> PopupMenu {
    let item = |label: String, f: Action<H>| {
        let host = host.clone();
        PopupMenuItem::new(label).on_click(move |_, window, cx| {
            if let Some(h) = host.upgrade() {
                h.update(cx, |h, cx| f(h, window, cx));
            }
        })
    };
    let label = |s: &str| s.to_string();
    menu.item(item(
        label("Copy Row as JSON"),
        Rc::new(move |h: &H, w, cx| {
            let g = h.export_grid().read(cx).delegate();
            let text = export::render(
                Format::Json,
                &g.columns,
                &[g.row_values(row)],
                &h.sql_target(cx),
            );
            // A single object, not a one-element array.
            let v: serde_json::Value = serde_json::from_str(&text).unwrap_or_default();
            let text = serde_json::to_string_pretty(&v[0]).unwrap_or_default();
            copy(text, "row as JSON", w, cx)
        }),
    ))
    .item(item(
        label("Copy Row as INSERT"),
        Rc::new(move |h: &H, w, cx| {
            let g = h.export_grid().read(cx).delegate();
            let text = export::inserts(&h.sql_target(cx), &g.columns, &[g.row_values(row)]);
            copy(text, "row as INSERT", w, cx)
        }),
    ))
    .item(item(
        label("Copy Row as UPDATE"),
        Rc::new(move |h: &H, w, cx| {
            let g = h.export_grid().read(cx).delegate();
            let text = export::updates(&h.sql_target(cx), &g.columns, &g.pk, &[g.row_values(row)]);
            copy(text, "row as UPDATE", w, cx)
        }),
    ))
    .item(item(
        label("Copy Column as IN (…)"),
        Rc::new(move |h: &H, w, cx| {
            let t = h.export_grid().read(cx);
            let col = t.selected_cell().map(|(_, c)| c).unwrap_or(0);
            let text = export::in_list(h.sql_target(cx).driver, &t.delegate().column_values(col));
            copy(text, "column values", w, cx)
        }),
    ))
    .separator()
    .item(item(
        label("Copy All as CSV"),
        Rc::new(|h: &H, w, cx| copy_all(h, Format::Csv, w, cx)),
    ))
    .item(item(
        label("Copy All as Markdown"),
        Rc::new(|h: &H, w, cx| copy_all(h, Format::Markdown, w, cx)),
    ))
    .item(item(
        label("Copy All as INSERT"),
        Rc::new(|h: &H, w, cx| copy_all(h, Format::SqlInsert, w, cx)),
    ))
    .item(item(
        label("Export…"),
        Rc::new(|h: &H, w, cx| open_export(h, w, cx)),
    ))
}

fn copy_all<H: ExportHost>(h: &H, f: Format, window: &mut Window, cx: &mut App) {
    let g = h.export_grid().read(cx).delegate();
    let text = export::render(f, &g.columns, &g.all_values(), &h.sql_target(cx));
    copy(text, &format!("as {}", f.label()), window, cx);
}

/// Picks a format, then a file, then writes every row there.
pub fn open_export<H: ExportHost>(h: &H, window: &mut Window, cx: &mut App) {
    let g = h.export_grid().read(cx).delegate();
    let columns = g.columns.clone();
    let loaded = g.all_values();
    let target = h.sql_target(cx);
    let name = h.export_name();
    let source = Rc::new(std::cell::RefCell::new(Some(h.export_source(cx))));
    let items = Format::ALL
        .iter()
        .map(|f| PickItem::new(f.extension(), f.label()))
        .collect();
    let on_pick: OnPick = Rc::new(move |id, window, cx| {
        let Some(format) = Format::from_id(&id) else {
            return;
        };
        let Some(source) = source.borrow_mut().take() else {
            return;
        };
        let dir = dirs::download_dir()
            .or_else(dirs::home_dir)
            .unwrap_or_else(|| PathBuf::from("."));
        let file = format!("{name}.{}", format.extension());
        let path = cx.prompt_for_new_path(&dir, Some(&file));
        let (columns, loaded, target) = (columns.clone(), loaded.clone(), target.clone());
        window
            .spawn(cx, async move |cx| {
                let Ok(Ok(Some(path))) = path.await else {
                    return;
                };
                let res = write_export(format, columns, loaded, target, source, path.clone()).await;
                cx.update(|window, cx| match res {
                    Ok(n) => {
                        let shown = path.display().to_string();
                        window.push_notification(
                            Notification::success(format!("Exported {n} rows to {shown}")),
                            cx,
                        );
                        cx.reveal_path(&path);
                    }
                    Err(e) => window.push_notification(
                        Notification::error(format!("Export failed: {e:#}")),
                        cx,
                    ),
                })
                .ok();
            })
            .detach();
    });
    Picker::open(
        "Export",
        "Choose a format…",
        IconName::FileText,
        items,
        on_pick,
        window,
        cx,
    );
}

/// Rows per page when exporting a whole table.
const PAGE: usize = 5000;

async fn write_export(
    format: Format,
    columns: Vec<String>,
    loaded: Vec<Row>,
    target: SqlTarget,
    source: Source,
    path: PathBuf,
) -> anyhow::Result<usize> {
    use std::io::Write as _;
    // Written next to the destination, then renamed: a failed or cancelled
    // export never leaves a half file under the chosen name.
    let tmp = path.with_extension(format!("tabula-{}.tmp", uuid::Uuid::new_v4().simple()));
    let result = runtime::spawn({
        let tmp = tmp.clone();
        async move {
            let mut out = std::io::BufWriter::new(std::fs::File::create(&tmp)?);
            match source {
                Source::Loaded => {
                    let mut e = export::Encoder::new(format, &columns, &target);
                    out.write_all(e.header().as_bytes())?;
                    out.write_all(e.rows(&loaded).as_bytes())?;
                    out.write_all(e.footer().as_bytes())?;
                    out.flush()?;
                    Ok(loaded.len())
                }
                // Page by page, so a big table is never all in memory.
                Source::Table { conn, query } => {
                    let c = conn.read().await;
                    let mut q = query;
                    q.limit = PAGE;
                    q.offset = 0;
                    let (first, _) = c.fetch_table(&q).await?;
                    let columns = first.columns.clone();
                    let mut e = export::Encoder::new(format, &columns, &target);
                    out.write_all(e.header().as_bytes())?;
                    let mut page = first.rows;
                    let mut n = 0;
                    loop {
                        n += page.len();
                        out.write_all(e.rows(&page).as_bytes())?;
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
            std::fs::rename(&tmp, &path)?;
            Ok(n)
        }
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
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

    /// A whole-table export reads every page, not just the one on screen.
    #[test]
    fn table_export_reads_all_pages() {
        let mut cfg = ConnectionConfig::new(Driver::Sqlite);
        let db = std::env::temp_dir().join(format!("tabula-export-{}.db", cfg.id));
        std::fs::write(&db, b"").unwrap();
        cfg.file = db.to_string_lossy().into();
        let out = std::env::temp_dir().join(format!("tabula-export-{}.csv", cfg.id));
        let out2 = out.clone();
        let n = futures::executor::block_on(runtime::spawn(async move {
            let c = Connection::connect(&cfg).await?;
            c.execute(
                "create table t (id integer primary key, v text); \
                 with recursive n(x) as (select 1 union all select x + 1 from n where x < 12345) \
                 insert into t (v) select 'row ' || x from n",
            )
            .await?;
            let conn = std::sync::Arc::new(tokio::sync::RwLock::new(c));
            let query = TableQuery {
                object: DbObject {
                    schema: None,
                    name: "t".into(),
                    kind: ObjectKind::Table,
                },
                filters: vec![],
                order: Some(("id".into(), true)),
                limit: 300,
                offset: 600,
            };
            let target = SqlTarget {
                driver: Driver::Sqlite,
                schema: None,
                table: "t".into(),
            };
            let source = Source::Table { conn, query };
            write_export(Format::Csv, vec![], vec![], target, source, out2).await
        }))
        .unwrap();
        assert_eq!(n, 12345);
        let text = std::fs::read_to_string(&out).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 12346);
        assert_eq!(lines[0], "id,v");
        assert_eq!(lines[1], "1,row 1");
        assert_eq!(lines[12345], "12345,row 12345");
        let _ = std::fs::remove_file(out);
        let _ = std::fs::remove_file(db);
    }
}
