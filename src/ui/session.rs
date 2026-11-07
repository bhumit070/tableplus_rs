//! One open connection: sidebar (database switcher, search, object tree) and
//! a tab strip of table and query tabs.

use std::rc::Rc;

use gpui::{
    AnyElement, App, Context, Entity, FocusHandle, Focusable, FontWeight, IntoElement,
    ParentElement, Render, Styled, Subscription, WeakEntity, Window, div, prelude::*, px,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::list::ListItem;
use gpui_component::menu::PopupMenuItem;
use gpui_component::notification::Notification;
use gpui_component::tree::{TreeItem, TreeState, tree};
use gpui_component::{
    ActiveTheme as _, Icon, Sizable as _, WindowExt as _, h_flex, h_resizable, resizable_panel,
    v_flex,
};
use gpui_kit_assets::IconName;

use super::Shared;
use super::completion::Schema;
use super::picker::{OnPick, PickItem, Picker};
use super::query_tab::QueryTab;
use super::table_tab::{TableTab, TableTabEvent};
use super::theme::{caps, dp, kbd, tokens};
use crate::db::{ConnectionConfig, DbObject, Driver, Filter, ObjectKind};
use crate::{history, runtime};
use anyhow::{Result, anyhow};
use dirs::{download_dir, home_dir};
use gpui::{Action, ClipboardItem, PathPromptOptions, transparent_black};
use gpui_component::menu::PopupMenu;
use tokio::{sync::RwLock, sync::RwLockWriteGuard, time::timeout};

enum TabItem {
    Table(Entity<TableTab>),
    Query(Entity<QueryTab>),
}

impl TabItem {
    /// Has staged edits (a table, or an editable query result).
    fn is_dirty(&self, cx: &App) -> bool {
        match self {
            TabItem::Table(t) => t.read(cx).is_dirty(cx),
            TabItem::Query(q) => q.read(cx).is_dirty(cx),
        }
    }
}

pub struct Session {
    pub shared: Rc<Shared>,
    objects: Vec<DbObject>,
    databases: Vec<String>,
    /// As reported by the connection; the dropdown widget isn't a reliable
    /// source for this (see GitHub #11).
    current_db: String,
    /// Postgres schemas in this database (empty for other drivers).
    schemas: Vec<String>,
    /// Shown schema; `None` = all.
    schema: Option<String>,
    /// The server's version, for the status bar, once known.
    server_version: Option<String>,
    /// (schema, table, column), for autocomplete.
    columns: Vec<(String, String, String)>,
    search: Entity<InputState>,
    tree: Entity<TreeState>,
    tabs: Vec<TabItem>,
    active: usize,
    queries_opened: usize,
    loading: bool,
    /// Why the table list couldn't be loaded; shown in the sidebar with Retry.
    load_error: Option<String>,
    /// Why the database list couldn't be loaded.
    dbs_error: Option<String>,
    focus: FocusHandle,
    _subs: Vec<Subscription>,
}

impl Session {
    pub fn new(shared: Rc<Shared>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Filter tables…"));
        let tree = cx.new(|cx| TreeState::new(cx));
        let subs = vec![
            cx.subscribe_in(&search, window, |this, _, ev: &InputEvent, _, cx| {
                if let InputEvent::Change = ev {
                    this.rebuild_tree(cx);
                }
            }),
        ];
        let mut this = Self {
            shared,
            objects: Vec::new(),
            databases: Vec::new(),
            current_db: String::new(),
            schemas: Vec::new(),
            schema: None,
            server_version: None,
            columns: Vec::new(),
            search,
            tree,
            tabs: Vec::new(),
            active: 0,
            queries_opened: 0,
            loading: false,
            load_error: None,
            dbs_error: None,
            focus: cx.focus_handle(),
            _subs: subs,
        };
        this.load_objects(window, cx);
        this.load_server_version(window, cx);
        this
    }

    /// For the status bar; failing to get it isn't worth a message.
    fn load_server_version(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let conn = self.shared.conn.clone();
        let driver = self.shared.cfg.driver;
        let task = runtime::spawn(async move { conn.read().await.server_version(driver).await });
        cx.spawn_in(window, async move |this, cx| {
            let version = task.await.ok();
            this.update(cx, |this, cx| {
                this.server_version = version;
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn load_objects(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.loading = true;
        let conn = self.shared.conn.clone();
        let task = runtime::spawn(async move {
            let c = conn.read().await;
            let objects = c.list_objects().await?;
            let dbs = c.databases().await.map_err(|e| format!("{e:#}"));
            // Autocomplete still works (minus columns) if this fails.
            let columns = c.all_columns().await.unwrap_or_default();
            Ok((
                objects,
                dbs,
                c.current_database(),
                columns,
                c.current_schema(),
            ))
        });
        cx.spawn_in(window, async move |this, cx| {
            let res = task.await;
            this.update_in(cx, |this, window, cx| {
                this.loading = false;
                match res {
                    Ok((objects, dbs, current, columns, schema)) => {
                        this.load_error = None;
                        let (dbs, dbs_error) = match dbs {
                            Ok(d) => (d, None),
                            Err(e) => (vec![], Some(e)),
                        };
                        this.dbs_error = dbs_error;
                        this.schemas = if this.shared.cfg.driver == Driver::Postgres {
                            schemas_of(&objects)
                        } else {
                            vec![]
                        };
                        this.schema = schema;
                        this.objects = objects;
                        this.columns = columns;
                        this.refresh_completion();
                        this.databases = dbs;
                        this.current_db = current;
                        this.rebuild_tree(cx);
                        // Redis has a single "keys" view: open it straight away.
                        if this.tabs.is_empty()
                            && this
                                .objects
                                .first()
                                .is_some_and(|o| o.kind == ObjectKind::Keys)
                        {
                            this.open_object(0, window, cx);
                        }
                    }
                    Err(e) => {
                        // An old list would look current; show the error instead.
                        this.objects.clear();
                        this.load_error = Some(format!("{e:#}"));
                        this.rebuild_tree(cx);
                        window.push_notification(
                            Notification::error(format!("{e:#}")).title("Couldn't load tables"),
                            cx,
                        );
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Switching closes table tabs, so confirm first if any has unsaved
    /// edits.
    fn request_switch_database(
        &mut self,
        name: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if name == self.current_db {
            return;
        }
        if !self.has_unsaved(cx) {
            self.switch_database(name, window, cx);
            return;
        }
        let me = cx.entity().downgrade();
        window.open_alert_dialog(cx, move |d, _, _| {
            let (ok, name) = (me.clone(), name.clone());
            d.title(format!("Switch to {name}?"))
                .description("Tables with uncommitted changes will be closed and the changes lost.")
                .show_cancel(true)
                .on_ok(move |_, window, cx| {
                    ok.update(cx, |s, cx| s.switch_database(name.clone(), window, cx))
                        .ok();
                    true
                })
        });
    }

    fn switch_database(&mut self, name: String, window: &mut Window, cx: &mut Context<Self>) {
        let conn = self.shared.conn.clone();
        let n = name.clone();
        let task = runtime::spawn(async move { exclusive(&conn).await?.use_database(&n).await });
        cx.spawn_in(window, async move |this, cx| {
            let res = task.await;
            this.update_in(cx, |this, window, cx| match res {
                Ok(()) => {
                    // Right away, not after the sidebar reload, so a quick
                    // ⌘K already knows which database is current.
                    this.current_db = name.clone();
                    // Table tabs belong to the old database.
                    this.tabs.retain(|t| matches!(t, TabItem::Query(_)));
                    // Query results can't be edited against the new one.
                    for t in &this.tabs {
                        if let TabItem::Query(q) = t {
                            q.update(cx, |q, cx| q.database_changed(cx));
                        }
                    }
                    this.active = 0;
                    this.load_objects(window, cx);
                    window.push_notification(format!("Switched to {name}"), cx);
                }
                Err(e) => window.push_notification(Notification::error(format!("{e:#}")), cx),
            })
            .ok();
        })
        .detach();
    }

    fn rebuild_tree(&mut self, cx: &mut Context<Self>) {
        let q = self.search.read(cx).value().to_string();
        let items = build_tree(
            &self.objects,
            &q,
            self.schema.as_deref(),
            self.schemas.len() > 1,
        );
        self.tree.update(cx, |t, cx| t.set_items(items, cx));
        cx.notify();
    }

    /// Autocomplete follows the selected schema (all schemas when none is).
    fn refresh_completion(&mut self) {
        let schema = self.schema.as_deref();
        let in_schema = |s: Option<&str>| schema.is_none() || s == schema;
        let names = self
            .objects
            .iter()
            .filter(|o| in_schema(o.schema.as_deref()))
            .map(|o| o.name.clone())
            .collect();
        let cols = self
            .columns
            .iter()
            .filter(|(s, _, _)| in_schema(Some(s)))
            .map(|(_, t, c)| (t.clone(), c.clone()))
            .collect();
        *self.shared.schema.borrow_mut() = Schema::new(names, cols);
    }

    /// Show one schema (or all, with `None`) and make it the default for
    /// unqualified names in queries (Postgres `search_path`).
    fn choose_schema(
        &mut self,
        schema: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if schema == self.schema {
            return;
        }
        // The sidebar changes only once queries really use the new schema,
        // so the two can't disagree if the reconnect fails.
        let conn = self.shared.conn.clone();
        let s = schema.clone();
        let task = runtime::spawn(async move { exclusive(&conn).await?.set_schema(s).await });
        cx.spawn_in(window, async move |this, cx| {
            let res = task.await;
            this.update_in(cx, |this, window, cx| match res {
                Ok(()) => {
                    this.schema = schema;
                    this.rebuild_tree(cx);
                    this.refresh_completion();
                }
                Err(e) => window.push_notification(
                    Notification::error(format!("{e:#}")).title("Couldn't switch schema"),
                    cx,
                ),
            })
            .ok();
        })
        .detach();
    }

    /// The title-bar breadcrumb's schema part: Postgres with more than one
    /// schema only. `None` inside means all schemas.
    pub fn schema_choice(&self) -> Option<Option<&str>> {
        (self.schemas.len() > 1).then_some(self.schema.as_deref())
    }

    /// The breadcrumb's schema menu: one schema, or all.
    pub fn open_schema_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let mut items = vec![PickItem {
            current: self.schema.is_none(),
            ..PickItem::new(ALL_SCHEMAS, ALL_SCHEMAS)
        }];
        items.extend(self.schemas.iter().map(|s| PickItem {
            current: self.schema.as_deref() == Some(s),
            ..PickItem::new(s, s)
        }));
        let me = cx.entity().downgrade();
        let on_pick: OnPick = Rc::new(move |name, window, cx| {
            let schema = (name != ALL_SCHEMAS).then_some(name);
            me.update(cx, |s, cx| s.choose_schema(schema, window, cx))
                .ok();
        });
        Picker::open(
            "Switch Schema",
            "Switch schema…",
            IconName::FolderTree,
            items,
            on_pick,
            window,
            cx,
        );
    }

    /// ⌘P: go to a table, view or collection here, or run a command.
    pub fn open_goto(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let mut items: Vec<PickItem> = self
            .objects
            .iter()
            .enumerate()
            .filter(|(_, o)| o.kind != ObjectKind::Keys)
            .map(|(i, o)| {
                let kind = match o.kind {
                    ObjectKind::View => "view",
                    ObjectKind::Collection => "collection",
                    _ => "table",
                };
                let detail = match &o.schema {
                    Some(s) => format!("{kind} · {s}"),
                    None => kind.to_string(),
                };
                PickItem {
                    detail: Some(detail),
                    ..PickItem::new(format!("o:{i}"), o.name.clone())
                }
            })
            .collect();
        for (i, (label, keys, _)) in goto_commands().iter().enumerate() {
            items.push(PickItem {
                detail: Some(format!("command · {keys}")),
                ..PickItem::new(format!("cmd:{i}"), *label)
            });
        }
        let me = cx.entity().downgrade();
        let on_pick: OnPick = Rc::new(move |id, window, cx| {
            if let Some(i) = id.strip_prefix("o:").and_then(|i| i.parse::<usize>().ok()) {
                me.update(cx, |s, cx| s.open_object(i, window, cx)).ok();
            } else if let Some(i) = id
                .strip_prefix("cmd:")
                .and_then(|i| i.parse::<usize>().ok())
            {
                window.dispatch_action((goto_commands()[i].2)(), cx);
            }
        });
        Picker::open(
            "Go to",
            "Go to table, view or command…",
            IconName::Search,
            items,
            on_pick,
            window,
            cx,
        );
    }

    pub fn cfg(&self) -> &ConnectionConfig {
        &self.shared.cfg
    }

    /// The database in use, for the title-bar breadcrumb.
    pub fn current_db(&self) -> &str {
        &self.current_db
    }

    pub fn server_version(&self) -> Option<&str> {
        self.server_version.as_deref()
    }

    pub fn open_object(&mut self, idx: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(obj) = self.objects.get(idx).cloned() else {
            return;
        };
        if let Some(i) = self
            .tabs
            .iter()
            .position(|t| matches!(t, TabItem::Table(e) if e.read(cx).object == obj))
        {
            self.activate(i, window, cx);
            return;
        }
        self.open_table_tab(obj, vec![], window, cx);
    }

    fn open_table_tab(
        &mut self,
        obj: DbObject,
        fixed: Vec<Filter>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let shared = self.shared.clone();
        let tab = cx.new(|cx| TableTab::new(shared, obj, fixed, window, cx));
        self._subs
            .push(cx.subscribe_in(&tab, window, Self::on_table_event));
        self.tabs.push(TabItem::Table(tab));
        let i = self.tabs.len() - 1;
        self.activate(i, window, cx);
    }

    fn on_table_event(
        &mut self,
        _: &Entity<TableTab>,
        ev: &TableTabEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match ev {
            TableTabEvent::OpenReference {
                schema,
                table,
                filters,
            } => {
                // The listed object, so the tab matches the sidebar's.
                let obj = self
                    .objects
                    .iter()
                    .find(|o| {
                        &o.name == table
                            && (schema.is_none() || o.schema == *schema || o.schema.is_none())
                    })
                    .cloned()
                    .unwrap_or_else(|| DbObject {
                        schema: schema.clone(),
                        name: table.clone(),
                        kind: ObjectKind::Table,
                    });
                self.open_table_tab(obj, filters.clone(), window, cx);
            }
            TableTabEvent::OpenQuery(sql) => self.open_query(sql.clone(), window, cx),
        }
    }

    pub fn new_query(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_query(String::new(), window, cx);
    }

    /// ⌘K: pick another database on this connection.
    pub fn open_db_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.databases.len() < 2 {
            match &self.dbs_error {
                Some(e) => window.push_notification(
                    Notification::error(e.clone()).title("Couldn't list databases"),
                    cx,
                ),
                None => window.push_notification("This connection has only one database", cx),
            }
            return;
        }
        let items = self
            .databases
            .iter()
            .map(|d| PickItem {
                current: *d == self.current_db,
                ..PickItem::new(d, d)
            })
            .collect();
        let me = cx.entity().downgrade();
        let on_pick: OnPick = Rc::new(move |name, window, cx| {
            me.update(cx, |s, cx| s.request_switch_database(name, window, cx))
                .ok();
        });
        Picker::open(
            "Switch Database",
            "Switch database…",
            IconName::Database,
            items,
            on_pick,
            window,
            cx,
        );
    }

    /// ⌘Y: past queries on this connection, newest first; picking one opens
    /// it in a new query tab.
    pub fn open_history(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let conn_id = self.shared.cfg.id.clone();
        let task = runtime::spawn(async move { history::recent(&conn_id, 1000).await });
        cx.spawn_in(window, async move |this, cx| {
            let entries = task.await;
            this.update_in(cx, |s, window, cx| match entries {
                Ok(entries) => s.show_history(entries, window, cx),
                Err(e) => window.push_notification(
                    Notification::error(format!("Couldn't read query history: {e:#}")),
                    cx,
                ),
            })
            .ok();
        })
        .detach();
    }

    fn show_history(
        &mut self,
        entries: Vec<history::Entry>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if entries.is_empty() {
            window.push_notification("No query history yet for this connection", cx);
            return;
        }
        let now = history::now();
        let mut items: Vec<PickItem> = entries
            .iter()
            .enumerate()
            .map(|(i, e)| {
                let one_line = e.sql.split_whitespace().collect::<Vec<_>>().join(" ");
                let mut detail = format!("{} · {}", history::ago(e.ran_at, now), e.database);
                match (&e.error, e.rows) {
                    (Some(_), _) => detail.push_str(" · failed"),
                    (None, Some(n)) => detail.push_str(&format!(" · {n} rows")),
                    _ => {}
                }
                PickItem {
                    detail: Some(detail),
                    ..PickItem::new(i.to_string(), one_line)
                }
            })
            .collect();
        items.push(PickItem::new("clear", "Clear history for this connection…"));
        let me = cx.entity().downgrade();
        let on_pick: OnPick = Rc::new(move |id, window, cx| {
            if id == "clear" {
                let me = me.clone();
                window.defer(cx, move |window, cx| {
                    me.update(cx, |s, cx| s.confirm_clear_history(window, cx))
                        .ok();
                });
                return;
            }
            let Some(sql) = id.parse::<usize>().ok().and_then(|i| entries.get(i)) else {
                return;
            };
            let sql = sql.sql.clone();
            me.update(cx, |s, cx| s.open_query(sql, window, cx)).ok();
        });
        Picker::open(
            "Query History",
            "Search past queries…",
            IconName::Undo2,
            items,
            on_pick,
            window,
            cx,
        );
    }

    /// ⌘⇧O: saved queries for this connection; picking one opens it.
    pub fn open_saved(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let list = match crate::saved::for_connection(&self.shared.cfg.id) {
            Ok(l) => l,
            Err(e) => {
                window.push_notification(
                    Notification::error(format!("{e:#}")).title("Saved queries"),
                    cx,
                );
                return;
            }
        };
        if list.is_empty() {
            window.push_notification("No saved queries yet: save one with ⌘⇧S", cx);
            return;
        }
        let item = |q: &crate::saved::SavedQuery| {
            let first = q.sql.split_whitespace().collect::<Vec<_>>().join(" ");
            let scope = if q.conn_id.is_none() {
                " · all connections"
            } else {
                ""
            };
            PickItem {
                detail: Some(format!(
                    "{}{scope}",
                    first.chars().take(60).collect::<String>()
                )),
                ..PickItem::new(q.id.clone(), q.name.clone())
            }
        };
        let mut items: Vec<PickItem> = list.iter().map(item).collect();
        items.push(PickItem::new("delete", "Delete a saved query…"));
        let me = cx.entity().downgrade();
        let on_pick: OnPick = Rc::new(move |id, window, cx| {
            let me = me.clone();
            if id == "delete" {
                window.defer(cx, move |window, cx| {
                    me.update(cx, |s, cx| s.pick_saved_to_delete(window, cx))
                        .ok();
                });
                return;
            }
            if let Some(q) = list.iter().find(|q| q.id == id).cloned() {
                me.update(cx, |s, cx| {
                    s.open_query(q.sql.clone(), window, cx);
                    if let Some(tab) = s.active_query() {
                        tab.update(cx, |t, _| t.set_saved(q));
                    }
                })
                .ok();
            }
        });
        Picker::open(
            "Saved Queries",
            "Search saved queries…",
            IconName::Star,
            items,
            on_pick,
            window,
            cx,
        );
    }

    fn pick_saved_to_delete(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let list = match crate::saved::for_connection(&self.shared.cfg.id) {
            Ok(l) => l,
            Err(e) => {
                window.push_notification(Notification::error(format!("{e:#}")), cx);
                return;
            }
        };
        let items = list
            .iter()
            .map(|q| PickItem::new(q.id.clone(), q.name.clone()))
            .collect();
        let on_pick: OnPick = Rc::new(move |id, window, cx| {
            let name = list
                .iter()
                .find(|q| q.id == id)
                .map(|q| q.name.clone())
                .unwrap_or_default();
            window.defer(cx, move |window, cx| {
                window.open_alert_dialog(cx, move |d, _, _| {
                    let id = id.clone();
                    d.title("Delete saved query?")
                        .description(format!("“{name}” will be removed."))
                        .show_cancel(true)
                        .on_ok(move |_, window, cx| {
                            match crate::saved::delete(&id) {
                                Ok(()) => window.push_notification("Deleted", cx),
                                Err(e) => window
                                    .push_notification(Notification::error(format!("{e:#}")), cx),
                            }
                            true
                        })
                });
            });
        });
        Picker::open(
            "Delete Saved Query",
            "Choose the query to delete…",
            IconName::Delete,
            items,
            on_pick,
            window,
            cx,
        );
    }

    fn confirm_clear_history(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let conn_id = self.shared.cfg.id.clone();
        let name = self.shared.cfg.display_name();
        window.open_alert_dialog(cx, move |d, _, _| {
            let conn_id = conn_id.clone();
            d.title("Clear query history?")
                .description(format!("Deletes every saved query run on {name}."))
                .on_ok(move |_, window, cx| {
                    let conn_id = conn_id.clone();
                    let task = runtime::spawn(async move { history::clear(&conn_id).await });
                    window
                        .spawn(cx, async move |cx| {
                            let res = task.await;
                            cx.update(|window, cx| match res {
                                Ok(()) => window.push_notification("Query history cleared", cx),
                                Err(e) => window
                                    .push_notification(Notification::error(format!("{e:#}")), cx),
                            })
                            .ok();
                        })
                        .detach();
                    true
                })
        });
    }

    pub fn active_table(&self) -> Option<Entity<TableTab>> {
        match self.tabs.get(self.active) {
            Some(TabItem::Table(t)) => Some(t.clone()),
            _ => None,
        }
    }

    pub fn active_query(&self) -> Option<Entity<QueryTab>> {
        match self.tabs.get(self.active) {
            Some(TabItem::Query(q)) => Some(q.clone()),
            _ => None,
        }
    }

    pub fn close_active_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.close_tab_at(self.active, window, cx);
    }

    /// `step` = 1 for next, -1 for previous; wraps around.
    pub fn cycle_tab(&mut self, step: isize, window: &mut Window, cx: &mut Context<Self>) {
        let n = self.tabs.len() as isize;
        if n > 0 {
            let i = (self.active as isize + step).rem_euclid(n) as usize;
            self.activate(i, window, cx);
        }
    }

    /// ⌘R: reload the open table, or the sidebar when no table is open.
    pub fn refresh(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.active_table() {
            // A failed sidebar retries too.
            Some(t) if self.load_error.is_none() => t.update(cx, |t, cx| t.refresh(window, cx)),
            _ => self.load_objects(window, cx),
        }
    }

    fn open_query(&mut self, initial: String, window: &mut Window, cx: &mut Context<Self>) {
        self.queries_opened += 1;
        let shared = self.shared.clone();
        let n = self.queries_opened;
        let tab = cx.new(|cx| QueryTab::new(shared, n, &initial, window, cx));
        self.tabs.push(TabItem::Query(tab.clone()));
        self.active = self.tabs.len() - 1;
        QueryTab::focus_editor(&tab, window, cx);
        cx.notify();
    }

    fn activate(&mut self, i: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.active = i;
        match self.tabs.get(i) {
            Some(TabItem::Table(t)) => window.focus(&t.focus_handle(cx), cx),
            Some(TabItem::Query(q)) => QueryTab::focus_editor(&q.clone(), window, cx),
            None => {}
        }
        cx.notify();
    }

    fn close_tab_at(&mut self, i: usize, window: &mut Window, cx: &mut Context<Self>) {
        if i >= self.tabs.len() {
            return;
        }
        {
            if self.tabs[i].is_dirty(cx) {
                let weak = cx.entity().downgrade();
                window.open_alert_dialog(cx, move |d, _, _| {
                    let weak = weak.clone();
                    d.title("Discard uncommitted changes?")
                        .description("This tab has edits that haven't been committed.")
                        .on_ok(move |_, window, cx| {
                            weak.update(cx, |s, cx| s.remove_tab(i, window, cx)).ok();
                            true
                        })
                });
                return;
            }
        }
        self.remove_tab(i, window, cx);
    }

    fn remove_tab(&mut self, i: usize, window: &mut Window, cx: &mut Context<Self>) {
        if i < self.tabs.len() {
            self.tabs.remove(i);
            // Keep showing the same tab when one to its left goes.
            if i < self.active {
                self.active -= 1;
            }
        }
        let next = self.active.min(self.tabs.len().saturating_sub(1));
        self.activate(next, window, cx);
    }

    /// Closes the table tabs showing `obj`, keeping the visible tab if it stays.
    fn close_tabs_of(&mut self, obj: &DbObject, cx: &App) {
        let current = self.tabs.get(self.active).map(|t| match t {
            TabItem::Table(e) => e.entity_id(),
            TabItem::Query(e) => e.entity_id(),
        });
        self.tabs
            .retain(|t| !matches!(t, TabItem::Table(e) if e.read(cx).object == *obj));
        self.active = current
            .and_then(|id| {
                self.tabs.iter().position(|t| match t {
                    TabItem::Table(e) => e.entity_id() == id,
                    TabItem::Query(e) => e.entity_id() == id,
                })
            })
            .unwrap_or(self.active)
            .min(self.tabs.len().saturating_sub(1));
    }

    /// What to reopen next launch.
    pub fn snapshot(&self, cx: &App) -> crate::restore::SessionState {
        use crate::restore::TabState;
        crate::restore::SessionState {
            conn_id: self.shared.cfg.id.clone(),
            tabs: self
                .tabs
                .iter()
                .map(|t| match t {
                    TabItem::Table(e) => {
                        let o = &e.read(cx).object;
                        TabState::Table {
                            schema: o.schema.clone(),
                            name: o.name.clone(),
                            kind: o.kind,
                        }
                    }
                    TabItem::Query(e) => {
                        let q = e.read(cx);
                        TabState::Query {
                            title: q.title.clone(),
                            text: q.text_now(cx),
                        }
                    }
                })
                .collect(),
            active_tab: self.active,
        }
    }

    /// Reopens tabs saved by `snapshot`.
    pub fn restore_tabs(
        &mut self,
        state: crate::restore::SessionState,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use crate::restore::TabState;
        for t in state.tabs {
            match t {
                TabState::Table { schema, name, kind } => {
                    self.open_table_tab(DbObject { schema, name, kind }, vec![], window, cx)
                }
                TabState::Query { title, text } => {
                    self.open_query(text, window, cx);
                    if let Some(q) = self.active_query() {
                        q.update(cx, |q, _| q.title = title);
                    }
                }
            }
        }
        if state.active_tab < self.tabs.len() {
            self.activate(state.active_tab, window, cx);
        }
        cx.notify();
    }

    #[cfg(test)]
    pub fn close_tab_for_test(&mut self, i: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.close_tab_at(i, window, cx);
    }

    #[cfg(test)]
    pub fn load_error(&self) -> Option<String> {
        self.load_error.clone()
    }

    #[cfg(test)]
    pub fn tab_count(&self) -> usize {
        self.tabs.len()
    }

    #[cfg(test)]
    pub fn object(&self, name: &str) -> Option<DbObject> {
        self.objects.iter().find(|o| o.name == name).cloned()
    }

    #[cfg(test)]
    pub fn object_index(&self, name: &str) -> Option<usize> {
        self.objects.iter().position(|o| o.name == name)
    }

    #[cfg(test)]
    pub fn new_query_for_test(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.new_query(window, cx);
    }

    #[cfg(test)]
    pub fn request_switch_for_test(
        &mut self,
        name: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.request_switch_database(name.to_string(), window, cx);
    }

    #[cfg(test)]
    pub fn database_count(&self) -> usize {
        self.databases.len()
    }

    pub fn has_unsaved(&self, cx: &App) -> bool {
        self.tabs.iter().any(|t| t.is_dirty(cx))
    }

    fn object_menu(session: WeakEntity<Session>, obj: DbObject, menu: PopupMenu) -> PopupMenu {
        let item = |label: &'static str, f: Rc<dyn Fn(&mut Session, &DbObject, &mut Window, &mut Context<Session>)>| {
            let s = session.clone();
            let obj = obj.clone();
            PopupMenuItem::new(label).on_click(move |_, window, cx| {
                let f = f.clone();
                let obj = obj.clone();
                s.update(cx, |this, cx| f(this, &obj, window, cx)).ok();
            })
        };
        menu.item(item(
            "Open",
            // By value: the list may have reloaded while the menu was open.
            Rc::new(|s, o, w, cx| match s.objects.iter().position(|x| x == o) {
                Some(i) => s.open_object(i, w, cx),
                None => w.push_notification(format!("{} is no longer in the list", o.title()), cx),
            }),
        ))
        .item(item(
            "Copy Name",
            Rc::new(|_, o, w, cx| {
                cx.write_to_clipboard(ClipboardItem::new_string(o.name.clone()));
                w.push_notification("Copied", cx);
            }),
        ))
        .item(item(
            "New Query for This",
            Rc::new(|s, o, w, cx| {
                let text = match s.shared.cfg.driver {
                    crate::db::Driver::Mongo => format!("db.{}.find({{}}).limit(100)", o.name),
                    crate::db::Driver::Redis => "SCAN 0 MATCH * COUNT 100".to_string(),
                    _ => format!("SELECT * FROM {} LIMIT 100;", o.title()),
                };
                s.open_query(text, w, cx);
            }),
        ))
        .separator()
        .item(item(
            "Edit Columns…",
            Rc::new(|s, o, w, cx| {
                if s.is_sql() {
                    s.edit_columns(o.clone(), w, cx)
                }
            }),
        ))
        .item(item(
            "New Table…",
            Rc::new(|s, _, w, cx| s.new_table(w, cx)),
        ))
        .item(item(
            "Rename…",
            Rc::new(|s, o, w, cx| {
                if s.is_sql() {
                    s.rename_object(o.clone(), w, cx)
                }
            }),
        ))
        .item(item(
            "Duplicate…",
            Rc::new(|s, o, w, cx| {
                if s.is_sql() && o.kind == ObjectKind::Table {
                    s.duplicate_object(o.clone(), w, cx)
                }
            }),
        ))
        .item(item(
            "Vacuum / Optimize",
            Rc::new(|s, o, w, cx| {
                if s.is_sql() && o.kind == ObjectKind::Table {
                    s.maintain_object(o.clone(), w, cx)
                }
            }),
        ))
        .separator()
        .item(item(
            "Truncate…",
            Rc::new(|s, o, w, cx| s.confirm_destructive(o.clone(), false, w, cx)),
        ))
        .item(item(
            "Drop…",
            Rc::new(|s, o, w, cx| s.confirm_destructive(o.clone(), true, w, cx)),
        ))
    }

    fn backup_target(&self) -> Option<crate::db::backup::Target> {
        let c = self.shared.conn.try_read().ok()?;
        let (host, port) = c.endpoint();
        Some(crate::db::backup::Target {
            cfg: self.shared.cfg.clone(),
            host,
            port,
            database: c.current_database(),
        })
    }

    /// Backup…: pick what to include, then where to save the dump.
    pub fn backup(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        use crate::db::backup::Scope;
        if !self.is_sql() {
            window.push_notification("Backups are available for Postgres, MySQL and SQLite", cx);
            return;
        }
        let Some(target) = self.backup_target() else {
            window.push_notification("The connection is busy; try again in a moment", cx);
            return;
        };
        let sqlite = self.shared.cfg.driver == Driver::Sqlite;
        let start = move |scope: Scope, window: &mut Window, cx: &mut App| {
            let ext = if sqlite { "sqlite" } else { "sql" };
            let date = chrono_like_date();
            let name = format!("{}-{date}.{ext}", target.database);
            let dir = download_dir().or_else(home_dir).unwrap_or_default();
            let path = cx.prompt_for_new_path(&dir, Some(&name));
            let target = target.clone();
            window
                .spawn(cx, async move |cx| {
                    let Ok(Ok(Some(path))) = path.await else {
                        return;
                    };
                    cx.update(|w, cx| w.push_notification("Backing up…", cx))
                        .ok();
                    let p2 = path.clone();
                    let res = runtime::spawn(async move {
                        crate::db::backup::dump(&target, scope, &p2).await
                    })
                    .await;
                    cx.update(|w, cx| match res {
                        Ok(()) => {
                            w.push_notification(
                                Notification::success(format!(
                                    "Backup saved to {}",
                                    path.display()
                                )),
                                cx,
                            );
                            cx.reveal_path(&path);
                        }
                        Err(e) => w.push_notification(
                            Notification::error(format!("{e:#}")).title("Backup failed"),
                            cx,
                        ),
                    })
                    .ok();
                })
                .detach();
        };
        if sqlite {
            start(Scope::Everything, window, cx);
            return;
        }
        let items = Scope::ALL
            .iter()
            .enumerate()
            .map(|(i, s)| PickItem::new(i.to_string(), s.label()))
            .collect();
        let on_pick: OnPick = Rc::new(move |id, window, cx| {
            if let Some(scope) = id.parse::<usize>().ok().and_then(|i| Scope::ALL.get(i)) {
                start(*scope, window, cx);
            }
        });
        Picker::open(
            "Backup",
            "What to include…",
            IconName::HardDrive,
            items,
            on_pick,
            window,
            cx,
        );
    }

    /// Restore…: runs a SQL dump file against the current database.
    pub fn restore(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !matches!(self.shared.cfg.driver, Driver::Postgres | Driver::MySql) {
            window.push_notification("Restore is available for Postgres and MySQL", cx);
            return;
        }
        if let crate::db::safety::Gate::Block(why) = crate::db::safety::gate_write(&self.shared.cfg)
        {
            window.push_notification(Notification::warning(why), cx);
            return;
        }
        let Some(target) = self.backup_target() else {
            window.push_notification("The connection is busy; try again in a moment", cx);
            return;
        };
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Restore".into()),
        });
        let me = cx.entity().downgrade();
        cx.spawn_in(window, async move |_, cx| {
            let Ok(Ok(Some(paths))) = rx.await else {
                return;
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };
            cx.update(|window, cx| {
                let desc = format!(
                    "Runs every statement in {} against {} ({}). {}",
                    path.display(),
                    target.database,
                    target.cfg.display_name(),
                    if target.cfg.driver == Driver::Postgres {
                        "It runs in one transaction: if anything fails, nothing changes."
                    } else {
                        "It stops at the first error; statements before it stay applied."
                    }
                );
                let (target, me) = (target.clone(), me.clone());
                window.open_alert_dialog(cx, move |d, _, _| {
                    let (target, path, me) = (target.clone(), path.clone(), me.clone());
                    d.title("Restore into this database?")
                        .description(desc.clone())
                        .show_cancel(true)
                        .on_ok(move |_, window, cx| {
                            let (target, path, me) = (target.clone(), path.clone(), me.clone());
                            window.push_notification("Restoring…", cx);
                            window
                                .spawn(cx, async move |cx| {
                                    let res = runtime::spawn(async move {
                                        crate::db::backup::restore(&target, &path).await
                                    })
                                    .await;
                                    cx.update(|w, cx| {
                                        match res {
                                            Ok(()) => w.push_notification(
                                                Notification::success("Restore finished"),
                                                cx,
                                            ),
                                            Err(e) => w.push_notification(
                                                Notification::error(format!("{e:#}"))
                                                    .title("Restore failed"),
                                                cx,
                                            ),
                                        }
                                        me.update(cx, |s, cx| s.load_objects(w, cx)).ok();
                                    })
                                    .ok();
                                })
                                .detach();
                            true
                        })
                });
            })
            .ok();
        })
        .detach();
    }

    /// Runs a DB call, then reports and reloads the sidebar.
    fn run_op(
        &mut self,
        done: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
        on_ok: impl FnOnce(&mut Self, &mut Context<Self>) + 'static,
        op: impl FnOnce(
            std::sync::Arc<RwLock<crate::db::Connection>>,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<Option<String>>> + Send>,
        >,
    ) {
        let task = runtime::spawn(op(self.shared.conn.clone()));
        cx.spawn_in(window, async move |this, cx| {
            let res = task.await;
            this.update_in(cx, |s, window, cx| {
                match res {
                    Ok(msg) => {
                        on_ok(s, cx);
                        window.push_notification(
                            Notification::success(msg.unwrap_or_else(|| done.to_string())),
                            cx,
                        )
                    }
                    Err(e) => window.push_notification(
                        Notification::error(format!("{e:#}")).title("Failed"),
                        cx,
                    ),
                }
                s.load_objects(window, cx);
            })
            .ok();
        })
        .detach();
    }

    fn blocked(&self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        match crate::db::safety::gate_write(&self.shared.cfg) {
            crate::db::safety::Gate::Block(why) => {
                window.push_notification(Notification::warning(why), cx);
                true
            }
            _ => false,
        }
    }

    pub fn new_database(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !matches!(self.shared.cfg.driver, Driver::Postgres | Driver::MySql) {
            window.push_notification("New databases are for Postgres and MySQL", cx);
            return;
        }
        if self.blocked(window, cx) {
            return;
        }
        let me = cx.entity().downgrade();
        super::prompt_text(
            "New Database",
            "",
            "",
            window,
            cx,
            move |name, window, cx| {
                me.update(cx, |s, cx| {
                    s.run_op(
                        "Database created",
                        window,
                        cx,
                        |_, _| {},
                        move |c| {
                            Box::pin(async move {
                                c.read().await.create_database(&name).await.map(|_| None)
                            })
                        },
                    )
                })
                .ok();
                true
            },
        );
    }

    /// Drop Database…: pick one (not the current), then type its name.
    pub fn drop_database(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !matches!(self.shared.cfg.driver, Driver::Postgres | Driver::MySql)
            || self.blocked(window, cx)
        {
            return;
        }
        let items: Vec<PickItem> = self
            .databases
            .iter()
            .filter(|d| **d != self.current_db)
            .map(|d| PickItem::new(d, d))
            .collect();
        if items.is_empty() {
            window.push_notification("No other database to drop", cx);
            return;
        }
        let me = cx.entity().downgrade();
        let on_pick: OnPick = Rc::new(move |name, window, cx| {
            let me = me.clone();
            window.defer(cx, move |window, cx| {
                let n2 = name.clone();
                super::prompt_text(
                    "Drop Database?",
                    &format!("This deletes {name} and everything in it. Type its name to confirm."),
                    "",
                    window,
                    cx,
                    move |typed, window, cx| {
                        if typed != n2 {
                            window.push_notification("The name doesn't match", cx);
                            return false;
                        }
                        me.update(cx, |s, cx| {
                            s.run_op(
                                "Database dropped",
                                window,
                                cx,
                                |_, _| {},
                                move |c| {
                                    Box::pin(async move {
                                        c.read().await.drop_database(&typed).await.map(|_| None)
                                    })
                                },
                            )
                        })
                        .ok();
                        true
                    },
                );
            });
        });
        Picker::open(
            "Drop Database",
            "Choose a database…",
            IconName::Delete,
            items,
            on_pick,
            window,
            cx,
        );
    }

    pub(crate) fn rename_object(
        &mut self,
        obj: DbObject,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.blocked(window, cx) {
            return;
        }
        let me = cx.entity().downgrade();
        let old = obj.name.clone();
        let initial = old.clone();
        super::prompt_text(
            &format!("Rename {}", obj.title()),
            "",
            &initial,
            window,
            cx,
            move |name, window, cx| {
                if name == old {
                    return true;
                }
                let o = obj.clone();
                me.update(cx, |s, cx| {
                    // Once renamed, tabs of the old name point at nothing.
                    let old = o.clone();
                    s.run_op(
                        "Renamed",
                        window,
                        cx,
                        move |s, cx| s.close_tabs_of(&old, cx),
                        move |c| {
                            Box::pin(async move {
                                c.read().await.rename_object(&o, &name).await.map(|_| None)
                            })
                        },
                    )
                })
                .ok();
                true
            },
        );
    }

    fn duplicate_object(&mut self, obj: DbObject, window: &mut Window, cx: &mut Context<Self>) {
        if self.blocked(window, cx) {
            return;
        }
        let me = cx.entity().downgrade();
        let suggested = format!("{}_copy", obj.name);
        super::prompt_text(
            &format!("Duplicate {}", obj.title()),
            "New table name (structure and rows are copied):",
            &suggested,
            window,
            cx,
            move |name, window, cx| {
                let o = obj.clone();
                me.update(cx, |s, cx| {
                    s.run_op(
                        "Table duplicated",
                        window,
                        cx,
                        |_, _| {},
                        move |c| {
                            Box::pin(async move {
                                c.read()
                                    .await
                                    .duplicate_table(&o, &name, true)
                                    .await
                                    .map(|_| None)
                            })
                        },
                    )
                })
                .ok();
                true
            },
        );
    }

    fn maintain_object(&mut self, obj: DbObject, window: &mut Window, cx: &mut Context<Self>) {
        if self.blocked(window, cx) {
            return;
        }
        window.push_notification("Working…", cx);
        self.run_op(
            "Done",
            window,
            cx,
            |_, _| {},
            move |c| Box::pin(async move { c.read().await.maintain(&obj).await.map(Some) }),
        );
    }

    fn is_sql(&self) -> bool {
        !matches!(self.shared.cfg.driver, Driver::Redis | Driver::Mongo)
    }

    /// New Table…: opens the table editor; the sidebar reloads afterwards.
    pub fn new_table(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.is_sql() {
            return;
        }
        let me = cx.entity().downgrade();
        let on_done: super::table_editor::OnDone = Rc::new(move |window, cx| {
            me.update(cx, |s, cx| s.load_objects(window, cx)).ok();
        });
        super::table_editor::TableEditor::open(
            self.shared.clone(),
            None,
            vec![],
            self.schema.clone(),
            on_done,
            window,
            cx,
        );
    }

    #[cfg(test)]
    pub fn new_table_for_test(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<super::table_editor::TableEditor> {
        let me = cx.entity().downgrade();
        let on_done: super::table_editor::OnDone = Rc::new(move |window, cx| {
            me.update(cx, |s, cx| s.load_objects(window, cx)).ok();
        });
        super::table_editor::TableEditor::open(
            self.shared.clone(),
            None,
            vec![],
            None,
            on_done,
            window,
            cx,
        )
    }

    /// Edit Columns… from the sidebar: loads the columns, then opens the editor.
    fn edit_columns(&mut self, obj: DbObject, window: &mut Window, cx: &mut Context<Self>) {
        let conn = self.shared.conn.clone();
        let o = obj.clone();
        let task = runtime::spawn(async move { conn.read().await.structure(&o).await });
        cx.spawn_in(window, async move |this, cx| {
            let res = task.await;
            this.update_in(cx, |s, window, cx| match res {
                Ok(st) => {
                    let me = cx.entity().downgrade();
                    let o2 = obj.clone();
                    let on_done: super::table_editor::OnDone = Rc::new(move |window, cx| {
                        me.update(cx, |s, cx| s.reload_table_tabs(&o2, window, cx))
                            .ok();
                    });
                    super::table_editor::TableEditor::open(
                        s.shared.clone(),
                        Some(obj),
                        st.columns,
                        None,
                        on_done,
                        window,
                        cx,
                    );
                }
                Err(e) => window.push_notification(Notification::error(format!("{e:#}")), cx),
            })
            .ok();
        })
        .detach();
    }

    /// After a structure change: open tabs of `obj` reload their columns.
    fn reload_table_tabs(&mut self, obj: &DbObject, window: &mut Window, cx: &mut Context<Self>) {
        for t in &self.tabs {
            if let TabItem::Table(e) = t
                && e.read(cx).object == *obj
            {
                e.update(cx, |t, cx| t.reload_structure(cx));
            }
        }
        self.load_objects(window, cx);
    }

    fn confirm_destructive(
        &mut self,
        obj: DbObject,
        drop: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let crate::db::safety::Gate::Block(why) = crate::db::safety::gate_write(&self.shared.cfg)
        {
            window.push_notification(Notification::warning(why), cx);
            return;
        }
        let weak = cx.entity().downgrade();
        let verb = if drop { "Drop" } else { "Truncate" };
        let title = format!("{verb} {}?", obj.title());
        window.open_alert_dialog(cx, move |d, _, _| {
            let weak = weak.clone();
            let obj = obj.clone();
            d.title(title.clone())
                .description("This can't be undone.")
                .on_ok(move |_, window, cx| {
                    let obj = obj.clone();
                    weak.update(cx, |s, cx| {
                        let conn = s.shared.conn.clone();
                        let o = obj.clone();
                        let task = runtime::spawn(async move {
                            let c = conn.read().await;
                            if drop {
                                c.drop_object(&o).await
                            } else {
                                c.truncate(&o).await
                            }
                        });
                        cx.spawn_in(window, async move |this, cx| {
                            let res = task.await;
                            this.update_in(cx, |this, window, cx| match res {
                                Ok(()) => {
                                    if drop {
                                        this.close_tabs_of(&obj, cx);
                                    }
                                    this.load_objects(window, cx);
                                    window.push_notification(Notification::success("Done"), cx);
                                }
                                Err(e) => window
                                    .push_notification(Notification::error(format!("{e:#}")), cx),
                            })
                            .ok();
                        })
                        .detach();
                    })
                    .ok();
                    true
                })
        });
    }

    fn render_sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let t = tokens(cx);
        let session = cx.entity().downgrade();
        let s2 = session.clone();
        let kinds: Rc<Vec<ObjectKind>> = Rc::new(self.objects.iter().map(|o| o.kind).collect());
        v_flex()
            .size_full()
            .bg(t.sidebar)
            .child(
                h_flex()
                    .gap(dp(4.))
                    .p(dp(12.))
                    .pb(dp(8.))
                    .child(
                        div().flex_1().min_w_0().child(
                            Input::new(&self.search)
                                .small()
                                .cleanable(true)
                                .prefix(Icon::new(IconName::Search).small()),
                        ),
                    )
                    .child(
                        Button::new("reload-objects")
                            .cursor_pointer()
                            .icon(IconName::RefreshCw)
                            .ghost()
                            .small()
                            .loading(self.loading)
                            .tooltip("Reload tables")
                            .on_click(
                                cx.listener(|this, _, window, cx| this.load_objects(window, cx)),
                            ),
                    ),
            )
            .when_some(self.load_error.clone(), |d, e| {
                d.child(
                    v_flex()
                        .gap(dp(6.))
                        .mx(dp(12.))
                        .mb(dp(8.))
                        .p(dp(10.))
                        .rounded(dp(8.))
                        .bg(t.danger.opacity(0.10))
                        .border_1()
                        .border_color(t.danger.opacity(0.3))
                        .text_size(dp(12.))
                        .text_color(t.danger)
                        .child("Couldn't load tables")
                        .child(div().text_color(t.text_secondary).child(e))
                        .child(
                            Button::new("retry-objects")
                                .cursor_pointer()
                                .debug_selector(|| "retry-objects".into())
                                .label("Retry")
                                .xsmall()
                                .loading(self.loading)
                                .on_click(
                                    cx.listener(|s, _, window, cx| s.load_objects(window, cx)),
                                ),
                        ),
                )
            })
            .child(
                div().flex_1().min_h_0().px(dp(6.)).child(
                    tree(&self.tree, move |ix, entry, selected, _, cx| {
                        let t = tokens(cx);
                        let id = entry.item().id.clone();
                        let label = entry.item().label.to_string();
                        let s = session.clone();
                        let row = ListItem::new(ix)
                            .cursor_pointer()
                            .h(dp(28.))
                            .py_0()
                            .rounded(dp(6.))
                            .pl(dp(8.) + dp(14.) * entry.depth() as f32);
                        if entry.is_folder() {
                            // "Tables (14)" → a caps header with the count on the right.
                            let (name, count) = match label.rsplit_once(" (") {
                                Some((n, c)) => {
                                    (n.to_string(), c.trim_end_matches(')').to_string())
                                }
                                None => (label.clone(), String::new()),
                            };
                            // Schema folders keep their name as typed.
                            let schema = id.starts_with("s:");
                            return row.pr(dp(8.)).child(
                                h_flex()
                                    .w_full()
                                    .gap(dp(6.))
                                    .child(
                                        Icon::new(if entry.is_expanded() {
                                            IconName::ChevronDown
                                        } else {
                                            IconName::ChevronRight
                                        })
                                        .xsmall()
                                        .text_color(t.text_faint),
                                    )
                                    .child(if schema {
                                        div()
                                            .text_size(dp(12.))
                                            .font_weight(FontWeight::MEDIUM)
                                            .text_color(t.text_secondary)
                                            .child(name)
                                    } else {
                                        caps(&name, cx)
                                    })
                                    .child(div().flex_1())
                                    .child(
                                        div()
                                            .font_family(cx.theme().mono_font_family.clone())
                                            .text_size(dp(11.))
                                            .text_color(t.text_faint)
                                            .child(count),
                                    ),
                            );
                        }
                        let kind = id
                            .strip_prefix("o:")
                            .and_then(|n| n.parse::<usize>().ok())
                            .and_then(|i| kinds.get(i).copied());
                        let (icon, color) = match kind {
                            Some(ObjectKind::View) => (IconName::Eye, t.keyword),
                            Some(ObjectKind::Collection) => (IconName::Layers, t.text_muted),
                            Some(ObjectKind::Keys) => (IconName::KeyRound, t.text_muted),
                            _ => (IconName::Table, t.text_muted),
                        };
                        row.selected(selected)
                            .pr(dp(8.))
                            .child(
                                h_flex()
                                    .gap(dp(8.))
                                    .text_size(dp(13.))
                                    .text_color(if selected { t.text } else { t.text_secondary })
                                    .child(Icon::new(icon).small().text_color(if selected {
                                        t.accent
                                    } else {
                                        color
                                    }))
                                    .child(div().truncate().child(label)),
                            )
                            .on_click(move |_, window, cx| {
                                if let Some(i) =
                                    id.strip_prefix("o:").and_then(|n| n.parse::<usize>().ok())
                                {
                                    s.update(cx, |s, cx| s.open_object(i, window, cx)).ok();
                                }
                            })
                    })
                    .context_menu(move |_, entry, menu, _, cx| {
                        let Some(i) = entry
                            .item()
                            .id
                            .strip_prefix("o:")
                            .and_then(|n| n.parse::<usize>().ok())
                        else {
                            return menu;
                        };
                        match s2
                            .upgrade()
                            .and_then(|s| s.read(cx).objects.get(i).cloned())
                        {
                            Some(obj) => Session::object_menu(s2.clone(), obj, menu),
                            None => menu,
                        }
                    }),
                ),
            )
            .when(self.is_sql(), |d| {
                d.child(
                    h_flex()
                        .px(dp(8.))
                        .py(dp(6.))
                        .border_t_1()
                        .border_color(t.border)
                        .child(
                            Button::new("new-table")
                                .cursor_pointer()
                                .icon(IconName::Plus)
                                .label("New table")
                                .ghost()
                                .xsmall()
                                .on_click(cx.listener(|s, _, window, cx| s.new_table(window, cx))),
                        ),
                )
            })
    }

    /// The tab strip: the active tab is "lifted" onto the canvas below it
    /// (design.md, screen 01).
    fn render_tabs(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let t = tokens(cx);
        let tabs: Vec<AnyElement> = self
            .tabs
            .iter()
            .enumerate()
            .map(|(i, tab)| {
                let (title, icon, icon_color, dirty) = match tab {
                    TabItem::Table(e) => (
                        e.read(cx).title(),
                        IconName::Table,
                        t.text_muted,
                        e.read(cx).is_dirty(cx),
                    ),
                    TabItem::Query(e) => (
                        e.read(cx).title.clone(),
                        IconName::SquareTerminal,
                        t.number,
                        e.read(cx).is_dirty(cx),
                    ),
                };
                let active = i == self.active;
                h_flex()
                    .id(("tab", i))
                    .group("tab")
                    .flex_none()
                    .h(dp(32.))
                    .max_w(dp(220.))
                    .pl(dp(12.))
                    .pr(dp(6.))
                    .gap(dp(8.))
                    .rounded_t(dp(8.))
                    .border_1()
                    .cursor_pointer()
                    .text_size(dp(12.))
                    .font_weight(FontWeight::MEDIUM)
                    .map(|d| {
                        if active {
                            // Covers the strip's bottom border, so the tab
                            // and the toolbar below read as one surface.
                            d.bg(t.canvas)
                                .border_color(t.border)
                                .border_b_0()
                                .mb(px(-1.))
                                .pb(px(1.))
                                .text_color(t.text)
                        } else {
                            d.border_color(transparent_black())
                                .text_color(t.text_secondary)
                                .hover(|d| d.bg(t.hover.opacity(0.6)))
                        }
                    })
                    .child(Icon::new(icon).small().text_color(icon_color))
                    .child(div().min_w_0().truncate().child(title))
                    .child(
                        div()
                            .id(("close-tab", i))
                            .size(dp(18.))
                            .flex_none()
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(dp(4.))
                            .cursor_pointer()
                            .text_color(t.text_muted)
                            .hover(|d| d.bg(t.hover).text_color(t.text))
                            .map(|d| {
                                if dirty {
                                    // A dot until hovered, then ×.
                                    d.child(
                                        div()
                                            .size(dp(7.))
                                            .rounded_full()
                                            .bg(t.text_secondary)
                                            .group_hover("tab", |d| d.invisible()),
                                    )
                                    .child(
                                        div()
                                            .absolute()
                                            .invisible()
                                            .group_hover("tab", |d| d.visible())
                                            .child(Icon::new(IconName::X).xsmall()),
                                    )
                                    .relative()
                                } else {
                                    d.child(Icon::new(IconName::X).xsmall()).when(!active, |d| {
                                        d.invisible().group_hover("tab", |d| d.visible())
                                    })
                                }
                            })
                            .on_click(cx.listener(move |this, _, window, cx| {
                                cx.stop_propagation();
                                this.close_tab_at(i, window, cx);
                            })),
                    )
                    .on_click(cx.listener(move |this, _, window, cx| this.activate(i, window, cx)))
                    .into_any_element()
            })
            .collect();
        h_flex()
            .flex_none()
            .h(dp(40.))
            .items_end()
            .bg(t.rail)
            .border_b_1()
            .border_color(t.border)
            .child(
                h_flex()
                    .id("session-tabs")
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .items_end()
                    .pl(dp(8.))
                    .gap(dp(2.))
                    .overflow_x_scroll()
                    .children(tabs),
            )
            .child(
                div().h_full().flex().items_center().px(dp(6.)).child(
                    Button::new("new-query")
                        .cursor_pointer()
                        .icon(IconName::Plus)
                        .ghost()
                        .small()
                        .tooltip("New query (⌘T)")
                        .on_click(cx.listener(|this, _, window, cx| this.new_query(window, cx))),
                ),
            )
    }

    fn render_empty(&self, cx: &mut Context<Self>) -> AnyElement {
        let t = tokens(cx);
        let hint = |keys: &'static str, what: &'static str, cx: &App| {
            h_flex()
                .gap(dp(8.))
                .child(kbd(keys, cx))
                .child(div().text_color(t.text_muted).child(what))
        };
        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .gap(dp(14.))
            .text_size(dp(12.))
            .child(
                div()
                    .text_size(dp(16.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(t.text)
                    .child(if self.loading {
                        "Loading…"
                    } else {
                        "Open a table from the sidebar"
                    }),
            )
            .child(
                h_flex()
                    .gap(dp(18.))
                    .child(hint("⌘T", "New query", cx))
                    .child(hint("⌘P", "Go to…", cx))
                    .child(hint("⌘K", "Switch database", cx))
                    .child(hint("⌘0", "Connections", cx)),
            )
            .into_any_element()
    }

    /// The right side of the status bar: what the active tab shows.
    pub fn status_context(&self, cx: &App) -> Option<String> {
        match self.tabs.get(self.active)? {
            TabItem::Table(t) => Some(t.read(cx).status_line(cx)),
            TabItem::Query(q) => q.read(cx).status_line(),
        }
    }
}

impl Focusable for Session {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for Session {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let content = match self.tabs.get(self.active) {
            Some(TabItem::Table(t)) => t.clone().into_any_element(),
            Some(TabItem::Query(q)) => q.clone().into_any_element(),
            None => self.render_empty(cx),
        };
        let t = tokens(cx);
        v_flex()
            .size_full()
            .key_context("Session")
            .track_focus(&self.focus)
            .child(
                h_resizable("session-split")
                    .child(
                        resizable_panel()
                            .size(px(260.))
                            .size_range(px(200.)..px(400.))
                            .child(self.render_sidebar(cx)),
                    )
                    .child(
                        resizable_panel().child(
                            v_flex()
                                .size_full()
                                .bg(t.canvas)
                                .when(!self.tabs.is_empty(), |d| d.child(self.render_tabs(cx)))
                                .child(div().flex_1().min_h_0().child(content)),
                        ),
                    ),
            )
    }
}

/// Commands offered by ⌘P: label, shortcut, action.
fn goto_commands() -> [(&'static str, &'static str, fn() -> Box<dyn Action>); 10] {
    use super::*;
    [
        ("New query", "⌘T", || Box::new(NewQuery)),
        ("Query history", "⌘Y", || Box::new(QueryHistory)),
        ("Saved queries", "⌘⇧O", || Box::new(SavedQueries)),
        ("Switch database", "⌘K", || Box::new(SwitchDatabase)),
        ("Refresh", "⌘R", || Box::new(Refresh)),
        ("Export…", "⌘⇧E", || Box::new(ExportData)),
        ("Processes", "⌘⇧P", || Box::new(ShowProcesses)),
        ("Connections", "⌘0", || Box::new(GoHome)),
        ("Toggle light / dark", "⌘⇧L", || Box::new(ToggleTheme)),
        ("Settings", "⌘,", || Box::new(OpenSettings)),
    ]
}

const ALL_SCHEMAS: &str = "All schemas";

/// Distinct schemas, `public` first, then alphabetical.
fn schemas_of(objects: &[DbObject]) -> Vec<String> {
    let mut v: Vec<String> = objects.iter().filter_map(|o| o.schema.clone()).collect();
    v.sort_by_key(|s| (s != "public", s.clone()));
    v.dedup();
    v
}

/// Sidebar tree. With several schemas and none selected, objects are
/// grouped under a folder per schema; otherwise Tables / Views / … only.
fn build_tree(
    objects: &[DbObject],
    query: &str,
    schema: Option<&str>,
    many_schemas: bool,
) -> Vec<TreeItem> {
    let q = query.to_lowercase();
    let shown = |o: &DbObject| {
        (schema.is_none() || o.schema.as_deref() == schema)
            && (q.is_empty() || o.name.to_lowercase().contains(&q))
    };
    // Within a schema folder the schema prefix is redundant.
    let label = |o: &DbObject| {
        if many_schemas {
            o.name.clone()
        } else {
            o.title()
        }
    };
    let groups = |in_schema: Option<&str>, prefix: &str| -> Vec<TreeItem> {
        [
            (ObjectKind::Table, "tables", "Tables"),
            (ObjectKind::View, "views", "Views"),
            (ObjectKind::Collection, "collections", "Collections"),
            (ObjectKind::Keys, "keys", "Keys"),
        ]
        .into_iter()
        .filter_map(|(kind, id, name)| {
            let children: Vec<TreeItem> = objects
                .iter()
                .enumerate()
                .filter(|(_, o)| o.kind == kind && shown(o))
                .filter(|(_, o)| in_schema.is_none() || o.schema.as_deref() == in_schema)
                .map(|(i, o)| TreeItem::new(format!("o:{i}"), label(o)))
                .collect();
            (!children.is_empty()).then(|| {
                TreeItem::new(
                    format!("g:{prefix}{id}"),
                    format!("{name} ({})", children.len()),
                )
                .expanded(true)
                .children(children)
            })
        })
        .collect()
    };
    if !many_schemas || schema.is_some() {
        return groups(None, "");
    }
    schemas_of(objects)
        .into_iter()
        .filter_map(|s| {
            let children = groups(Some(&s), &format!("{s}:"));
            let count: usize = children.iter().map(|g| g.children.len()).sum();
            (count > 0).then(|| {
                TreeItem::new(format!("s:{s}"), format!("{s} ({count})"))
                    .expanded(s == "public")
                    .children(children)
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn obj(schema: &str, name: &str, kind: ObjectKind) -> DbObject {
        DbObject {
            schema: Some(schema.into()),
            name: name.into(),
            kind,
        }
    }

    fn labels(items: &[TreeItem]) -> Vec<String> {
        items.iter().map(|i| i.label.to_string()).collect()
    }

    fn objects() -> Vec<DbObject> {
        vec![
            obj("sales", "orders", ObjectKind::Table),
            obj("public", "users", ObjectKind::Table),
            obj("public", "active_users", ObjectKind::View),
            obj("audit", "log", ObjectKind::Table),
        ]
    }

    #[test]
    fn schemas_listed_public_first() {
        assert_eq!(schemas_of(&objects()), ["public", "audit", "sales"]);
    }

    #[test]
    fn groups_by_schema_when_none_selected() {
        let tree = build_tree(&objects(), "", None, true);
        assert_eq!(labels(&tree), ["public (2)", "audit (1)", "sales (1)"]);
        assert_eq!(labels(&tree[0].children), ["Tables (1)", "Views (1)"]);
        // Inside a schema folder, names aren't prefixed with the schema.
        assert_eq!(labels(&tree[2].children[0].children), ["orders"]);
        assert!(tree[0].is_expanded() && !tree[1].is_expanded());
    }

    #[test]
    fn selected_schema_shows_only_its_objects() {
        let tree = build_tree(&objects(), "", Some("sales"), true);
        assert_eq!(labels(&tree), ["Tables (1)"]);
        assert_eq!(labels(&tree[0].children), ["orders"]);
    }

    #[test]
    fn search_hides_empty_schemas_and_single_schema_stays_flat() {
        let tree = build_tree(&objects(), "user", None, true);
        assert_eq!(labels(&tree), ["public (2)"]);
        let flat = build_tree(&objects()[1..3], "", None, false);
        assert_eq!(labels(&flat), ["Tables (1)", "Views (1)"]);
    }
}

/// The connection for a change that needs it to itself (switching
/// database / schema). Waiting indefinitely behind a long query would also
/// stall every other read queued after us, so give up after a few seconds.
async fn exclusive(
    conn: &std::sync::Arc<RwLock<crate::db::Connection>>,
) -> Result<RwLockWriteGuard<'_, crate::db::Connection>> {
    timeout(std::time::Duration::from_secs(3), conn.write())
        .await
        .map_err(|_| {
            anyhow!(
                "Something is still running on this connection (a query?). Stop it with ⌘. and try again."
            )
        })
}

/// Today as YYYY-MM-DD (UTC), for default file names.
fn chrono_like_date() -> String {
    date_from_days(crate::history::now() / 86_400)
}

fn date_from_days(days: i64) -> String {
    // Civil-from-days (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

#[cfg(test)]
mod date_tests {
    #[test]
    fn civil_date() {
        assert_eq!(super::date_from_days(0), "1970-01-01");
        assert_eq!(super::date_from_days(20762), "2026-11-05");
        assert_eq!(super::date_from_days(19782), "2024-02-29");
    }
}
