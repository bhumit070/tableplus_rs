//! One open connection: sidebar (database switcher, search, object tree) and
//! a tab strip of table and query tabs.

use std::rc::Rc;

use gpui::{
    AnyElement, App, Context, Entity, FocusHandle, Focusable, IntoElement, ParentElement, Render,
    Styled, Subscription, WeakEntity, Window, div, prelude::*, px, rems,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::list::ListItem;
use gpui_component::menu::PopupMenuItem;
use gpui_component::notification::Notification;
use gpui_component::select::{Select, SelectEvent, SelectState};
use gpui_component::tab::{Tab, TabBar};
use gpui_component::tree::{TreeItem, TreeState, tree};
use gpui_component::{
    ActiveTheme as _, Icon, IndexPath, Sizable as _, WindowExt as _, h_flex, h_resizable,
    resizable_panel, v_flex,
};
use gpui_kit_assets::IconName;

use super::completion::Schema;
use super::picker::{OnPick, PickItem, Picker};
use super::query_tab::QueryTab;
use super::table_tab::TableTab;
use super::{SearchList, Shared, search_list, tag_color};
use crate::db::{DbObject, Driver, ObjectKind};
use crate::runtime;

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
    db_select: Entity<SelectState<SearchList>>,
    /// Postgres schemas in this database (empty for other drivers).
    schemas: Vec<String>,
    /// Shown schema; `None` = all.
    schema: Option<String>,
    schema_select: Entity<SelectState<SearchList>>,
    /// (schema, table, column), for autocomplete.
    columns: Vec<(String, String, String)>,
    search: Entity<InputState>,
    tree: Entity<TreeState>,
    tabs: Vec<TabItem>,
    active: usize,
    queries_opened: usize,
    loading: bool,
    focus: FocusHandle,
    _subs: Vec<Subscription>,
}

impl Session {
    pub fn new(shared: Rc<Shared>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search tables…"));
        let db_select =
            cx.new(|cx| SelectState::new(search_list(vec![]), None, window, cx).searchable(true));
        let tree = cx.new(|cx| TreeState::new(cx));
        let schema_select =
            cx.new(|cx| SelectState::new(search_list(vec![]), None, window, cx).searchable(true));
        let subs = vec![
            cx.subscribe_in(
                &schema_select,
                window,
                |this, _, ev: &SelectEvent<SearchList>, window, cx| {
                    let SelectEvent::Confirm(Some(name)) = ev else {
                        return;
                    };
                    let schema = (name != ALL_SCHEMAS).then(|| name.clone());
                    this.choose_schema(schema, window, cx);
                },
            ),
            cx.subscribe_in(&search, window, |this, _, ev: &InputEvent, _, cx| {
                if let InputEvent::Change = ev {
                    this.rebuild_tree(cx);
                }
            }),
            cx.subscribe_in(
                &db_select,
                window,
                |this, _, ev: &SelectEvent<SearchList>, window, cx| {
                    let SelectEvent::Confirm(Some(name)) = ev else {
                        return;
                    };
                    this.request_switch_database(name.clone(), window, cx);
                },
            ),
        ];
        let mut this = Self {
            shared,
            objects: Vec::new(),
            databases: Vec::new(),
            current_db: String::new(),
            db_select,
            schemas: Vec::new(),
            schema: None,
            schema_select,
            columns: Vec::new(),
            search,
            tree,
            tabs: Vec::new(),
            active: 0,
            queries_opened: 0,
            loading: false,
            focus: cx.focus_handle(),
            _subs: subs,
        };
        this.load_objects(window, cx);
        this
    }

    fn load_objects(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.loading = true;
        let conn = self.shared.conn.clone();
        let task = runtime::spawn(async move {
            let c = conn.read().await;
            let objects = c.list_objects().await?;
            let dbs = c.databases().await.unwrap_or_default();
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
                        this.schemas = if this.shared.cfg.driver == Driver::Postgres {
                            schemas_of(&objects)
                        } else {
                            vec![]
                        };
                        this.schema = schema;
                        this.objects = objects;
                        this.columns = columns;
                        this.refresh_completion();
                        let mut choices = vec![ALL_SCHEMAS.to_string()];
                        choices.extend(this.schemas.iter().cloned());
                        let picked = this
                            .schema
                            .as_ref()
                            .and_then(|s| choices.iter().position(|c| c == s))
                            .unwrap_or(0);
                        this.schema_select.update(cx, |s, cx| {
                            s.set_items(search_list(choices), window, cx);
                            s.set_selected_index(Some(IndexPath::new(picked)), window, cx);
                        });
                        let sel = dbs.iter().position(|d| *d == current).map(IndexPath::new);
                        this.databases = dbs.clone();
                        this.current_db = current.clone();
                        this.db_select.update(cx, |s, cx| {
                            s.set_items(search_list(dbs), window, cx);
                            s.set_selected_index(sel, window, cx);
                        });
                        this.rebuild_tree(cx);
                        // Redis has a single "keys" view: open it straight away.
                        if this.tabs.is_empty()
                            && this.objects.len() == 1
                            && this.objects[0].kind == ObjectKind::Keys
                        {
                            this.open_object(0, window, cx);
                        }
                    }
                    Err(e) => window.push_notification(Notification::error(format!("{e:#}")), cx),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Switching closes table tabs, so confirm first if any has unsaved
    /// edits. Cancelling puts the dropdown back on the current database.
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
            let (ok, cancel, name) = (me.clone(), me.clone(), name.clone());
            d.title(format!("Switch to {name}?"))
                .description("Tables with uncommitted changes will be closed and the changes lost.")
                .on_ok(move |_, window, cx| {
                    ok.update(cx, |s, cx| s.switch_database(name.clone(), window, cx))
                        .ok();
                    true
                })
                .on_cancel(move |_, window, cx| {
                    cancel
                        .update(cx, |s, cx| s.reset_db_select(window, cx))
                        .ok();
                    true
                })
        });
    }

    /// Point the sidebar dropdown back at the database actually in use.
    fn reset_db_select(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let ix = self
            .databases
            .iter()
            .position(|d| *d == self.current_db)
            .map(IndexPath::new);
        self.db_select
            .update(cx, |s, cx| s.set_selected_index(ix, window, cx));
    }

    fn switch_database(&mut self, name: String, window: &mut Window, cx: &mut Context<Self>) {
        let conn = self.shared.conn.clone();
        let n = name.clone();
        let task = runtime::spawn(async move { conn.write().await.use_database(&n).await });
        cx.spawn_in(window, async move |this, cx| {
            let res = task.await;
            this.update_in(cx, |this, window, cx| match res {
                Ok(()) => {
                    // Right away, not after the sidebar reload, so a quick
                    // ⌘K already knows which database is current.
                    this.current_db = name.clone();
                    // Table tabs belong to the old database.
                    this.tabs.retain(|t| matches!(t, TabItem::Query(_)));
                    this.active = 0;
                    this.load_objects(window, cx);
                    window.push_notification(format!("Switched to {name}"), cx);
                }
                Err(e) => {
                    this.reset_db_select(window, cx);
                    window.push_notification(Notification::error(format!("{e:#}")), cx)
                }
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
        let task = runtime::spawn(async move { conn.write().await.set_schema(s).await });
        cx.spawn_in(window, async move |this, cx| {
            let res = task.await;
            this.update_in(cx, |this, window, cx| match res {
                Ok(()) => {
                    this.schema = schema;
                    this.rebuild_tree(cx);
                    this.refresh_completion();
                }
                Err(e) => {
                    this.reset_schema_select(window, cx);
                    window.push_notification(
                        Notification::error(format!("{e:#}")).title("Couldn't switch schema"),
                        cx,
                    );
                }
            })
            .ok();
        })
        .detach();
    }

    /// Point the Schema dropdown back at the schema actually in use.
    fn reset_schema_select(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let ix = match &self.schema {
            None => 0,
            Some(s) => self
                .schemas
                .iter()
                .position(|x| x == s)
                .map_or(0, |i| i + 1),
        };
        self.schema_select.update(cx, |s, cx| {
            s.set_selected_index(Some(IndexPath::new(ix)), window, cx)
        });
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
        let shared = self.shared.clone();
        let tab = cx.new(|cx| TableTab::new(shared, obj, window, cx));
        self.tabs.push(TabItem::Table(tab));
        let i = self.tabs.len() - 1;
        self.activate(i, window, cx);
    }

    pub fn new_query(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_query(String::new(), window, cx);
    }

    /// ⌘K: pick another database on this connection.
    pub fn open_db_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.databases.len() < 2 {
            window.push_notification("This connection has only one database", cx);
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
            Some(t) => t.update(cx, |t, cx| t.refresh(window, cx)),
            None => self.load_objects(window, cx),
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
        }
        let next = self.active.min(self.tabs.len().saturating_sub(1));
        self.activate(next, window, cx);
    }

    #[cfg(test)]
    pub fn tab_count(&self) -> usize {
        self.tabs.len()
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
    pub fn current_db(&self) -> &str {
        &self.current_db
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

    fn object_menu(
        session: WeakEntity<Session>,
        obj: DbObject,
        idx: usize,
        menu: gpui_component::menu::PopupMenu,
    ) -> gpui_component::menu::PopupMenu {
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
            Rc::new(move |s, _, w, cx| s.open_object(idx, w, cx)),
        ))
        .item(item(
            "Copy Name",
            Rc::new(|_, o, w, cx| {
                cx.write_to_clipboard(gpui::ClipboardItem::new_string(o.name.clone()));
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
            "Truncate…",
            Rc::new(|s, o, w, cx| s.confirm_destructive(o.clone(), false, w, cx)),
        ))
        .item(item(
            "Drop…",
            Rc::new(|s, o, w, cx| s.confirm_destructive(o.clone(), true, w, cx)),
        ))
    }

    fn confirm_destructive(
        &mut self,
        obj: DbObject,
        drop: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
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
                            if drop { c.drop_object(&o).await } else { c.truncate(&o).await }
                        });
                        cx.spawn_in(window, async move |this, cx| {
                            let res = task.await;
                            this.update_in(cx, |this, window, cx| match res {
                                Ok(()) => {
                                    if drop {
                                        this.tabs.retain(|t| !matches!(t, TabItem::Table(e) if e.read(cx).object == obj));
                                        this.active = this.active.min(this.tabs.len().saturating_sub(1));
                                    }
                                    this.load_objects(window, cx);
                                    window.push_notification(Notification::success("Done"), cx);
                                }
                                Err(e) => window.push_notification(Notification::error(format!("{e:#}")), cx),
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
        let theme = cx.theme();
        let session = cx.entity().downgrade();
        let s2 = session.clone();
        v_flex()
            .size_full()
            .bg(theme.sidebar)
            .child(
                v_flex()
                    .gap_1()
                    .p_2()
                    .child(Select::new(&self.db_select).small().placeholder("Database"))
                    .when(self.schemas.len() > 1, |d| {
                        d.child(
                            Select::new(&self.schema_select)
                                .small()
                                .placeholder("Schema"),
                        )
                    })
                    .child(
                        Input::new(&self.search)
                            .small()
                            .cleanable(true)
                            .prefix(Icon::new(IconName::Search).small()),
                    ),
            )
            .child(
                div().flex_1().min_h_0().child(
                    tree(&self.tree, move |ix, entry, selected, _, cx| {
                        let id = entry.item().id.clone();
                        let label = entry.item().label.clone();
                        let icon = if entry.is_folder() {
                            if entry.is_expanded() {
                                IconName::ChevronDown
                            } else {
                                IconName::ChevronRight
                            }
                        } else {
                            IconName::Table
                        };
                        let s = session.clone();
                        ListItem::new(ix)
                            .selected(selected)
                            .py_0p5()
                            .pl(px(10.) + px(14.) * entry.depth() as f32)
                            .child(
                                h_flex()
                                    .gap_1p5()
                                    .text_sm()
                                    .child(
                                        Icon::new(icon)
                                            .small()
                                            .text_color(cx.theme().muted_foreground),
                                    )
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
                            Some(obj) => Session::object_menu(s2.clone(), obj, i, menu),
                            None => menu,
                        }
                    }),
                ),
            )
    }

    fn render_tabs(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let tabs: Vec<Tab> = self
            .tabs
            .iter()
            .enumerate()
            .map(|(i, t)| {
                let (title, icon, dirty) = match t {
                    TabItem::Table(e) => {
                        (e.read(cx).title(), IconName::Table, e.read(cx).is_dirty(cx))
                    }
                    TabItem::Query(e) => (
                        e.read(cx).title.clone(),
                        IconName::SquareTerminal,
                        e.read(cx).is_dirty(cx),
                    ),
                };
                Tab::new()
                    .prefix(Icon::new(icon).small().ml_2())
                    .label(if dirty { format!("{title} •") } else { title })
                    .suffix(
                        Button::new(("close-tab", i))
                            .icon(IconName::X)
                            .ghost()
                            .xsmall()
                            .on_click(cx.listener(move |this, _, window, cx| {
                                cx.stop_propagation();
                                this.close_tab_at(i, window, cx);
                            })),
                    )
            })
            .collect();
        TabBar::new("session-tabs")
            .small()
            .selected_index(self.active)
            .children(tabs)
            .suffix(
                Button::new("new-query")
                    .icon(IconName::Plus)
                    .ghost()
                    .small()
                    .tooltip("New query (⌘T)")
                    .on_click(cx.listener(|this, _, window, cx| this.new_query(window, cx))),
            )
            .on_click(cx.listener(|this, ix: &usize, window, cx| this.activate(*ix, window, cx)))
    }

    fn render_empty(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .gap_2()
            .text_color(theme.muted_foreground)
            .child(div().text_lg().child(if self.loading {
                "Loading…"
            } else {
                "Open a table from the sidebar"
            }))
            .child(
                div()
                    .text_sm()
                    .child("⌘T new query · ⌘R refresh · ⌘W close tab · ⌘0 connections"),
            )
            .into_any_element()
    }
}

impl Focusable for Session {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for Session {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let content = match self.tabs.get(self.active) {
            Some(TabItem::Table(t)) => t.clone().into_any_element(),
            Some(TabItem::Query(q)) => q.clone().into_any_element(),
            None => self.render_empty(cx),
        };
        let cfg = &self.shared.cfg;
        let header = h_flex()
            .h(rems(2.))
            .px_3()
            .gap_2()
            .border_b_1()
            .border_color(theme.border)
            .child(div().size(px(8.)).rounded_full().bg(tag_color(cfg)))
            .child(
                // Opens the connection picker: switch to another open
                // connection, open a saved one, or add a new one.
                Button::new("switch-connection")
                    .label(cfg.display_name())
                    .ghost()
                    .xsmall()
                    .dropdown_caret(true)
                    .tooltip("Switch connection (⌘⇧K)")
                    .on_click(|_, window, cx| {
                        window.dispatch_action(Box::new(super::OpenConnection), cx)
                    }),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(format!("{} · {}", cfg.driver.label(), cfg.tag.label())),
            )
            .child(div().flex_1())
            .child(
                Button::new("reload-objects")
                    .icon(IconName::RefreshCw)
                    .ghost()
                    .xsmall()
                    .tooltip("Reload tables")
                    .on_click(cx.listener(|this, _, window, cx| this.load_objects(window, cx))),
            );
        v_flex()
            .size_full()
            .key_context("Session")
            .track_focus(&self.focus)
            .child(header)
            .child(
                h_resizable("session-split")
                    .child(
                        resizable_panel()
                            .size(px(250.))
                            .size_range(px(160.)..px(520.))
                            .child(self.render_sidebar(cx)),
                    )
                    .child(
                        resizable_panel().child(
                            v_flex()
                                .size_full()
                                .when(!self.tabs.is_empty(), |d| d.child(self.render_tabs(cx)))
                                .child(div().flex_1().min_h_0().child(content)),
                        ),
                    ),
            )
    }
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
