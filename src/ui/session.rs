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
use super::query_tab::QueryTab;
use super::table_tab::TableTab;
use super::{Shared, tag_color};
use crate::db::{DbObject, ObjectKind};
use crate::runtime;

enum TabItem {
    Table(Entity<TableTab>),
    Query(Entity<QueryTab>),
}

pub struct Session {
    pub shared: Rc<Shared>,
    objects: Vec<DbObject>,
    databases: Vec<String>,
    /// As reported by the connection; the dropdown widget isn't a reliable
    /// source for this (see GitHub #11).
    current_db: String,
    db_select: Entity<SelectState<Vec<String>>>,
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
            cx.new(|cx| SelectState::new(Vec::<String>::new(), None, window, cx).searchable(true));
        let tree = cx.new(|cx| TreeState::new(cx));
        let subs = vec![
            cx.subscribe_in(&search, window, |this, _, ev: &InputEvent, _, cx| {
                if let InputEvent::Change = ev {
                    this.rebuild_tree(cx);
                }
            }),
            cx.subscribe_in(
                &db_select,
                window,
                |this, _, ev: &SelectEvent<Vec<String>>, window, cx| {
                    let SelectEvent::Confirm(Some(name)) = ev else {
                        return;
                    };
                    this.switch_database(name.clone(), window, cx);
                },
            ),
        ];
        let mut this = Self {
            shared,
            objects: Vec::new(),
            databases: Vec::new(),
            current_db: String::new(),
            db_select,
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
            Ok((objects, dbs, c.current_database(), columns))
        });
        cx.spawn_in(window, async move |this, cx| {
            let res = task.await;
            this.update_in(cx, |this, window, cx| {
                this.loading = false;
                match res {
                    Ok((objects, dbs, current, columns)) => {
                        let names = objects.iter().map(|o| o.name.clone()).collect();
                        *this.shared.schema.borrow_mut() = Schema::new(names, columns);
                        this.objects = objects;
                        let sel = dbs.iter().position(|d| *d == current).map(IndexPath::new);
                        this.databases = dbs.clone();
                        this.current_db = current.clone();
                        this.db_select.update(cx, |s, cx| {
                            s.set_items(dbs, window, cx);
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
                Err(e) => window.push_notification(Notification::error(format!("{e:#}")), cx),
            })
            .ok();
        })
        .detach();
    }

    fn rebuild_tree(&mut self, cx: &mut Context<Self>) {
        let q = self.search.read(cx).value().to_lowercase();
        let matches = |o: &DbObject| q.is_empty() || o.title().to_lowercase().contains(&q);
        let group = |kind: ObjectKind, id: &str, label: &str| -> Option<TreeItem> {
            let children: Vec<TreeItem> = self
                .objects
                .iter()
                .enumerate()
                .filter(|(_, o)| o.kind == kind && matches(o))
                .map(|(i, o)| TreeItem::new(format!("o:{i}"), o.title()))
                .collect();
            if children.is_empty() {
                return None;
            }
            Some(
                TreeItem::new(id.to_string(), format!("{label} ({})", children.len()))
                    .expanded(true)
                    .children(children),
            )
        };
        let items: Vec<TreeItem> = [
            group(ObjectKind::Table, "g:tables", "Tables"),
            group(ObjectKind::View, "g:views", "Views"),
            group(ObjectKind::Collection, "g:collections", "Collections"),
            group(ObjectKind::Keys, "g:keys", "Keys"),
        ]
        .into_iter()
        .flatten()
        .collect();
        self.tree.update(cx, |t, cx| t.set_items(items, cx));
        cx.notify();
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
        let current = self.current_db.clone();
        let me = cx.entity().downgrade();
        let on_pick: super::db_picker::OnPick = Rc::new(move |name, window, cx| {
            me.update(cx, |s, cx| s.switch_database(name, window, cx))
                .ok();
        });
        super::db_picker::DbPicker::open(self.databases.clone(), current, on_pick, window, cx);
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
        if let TabItem::Table(t) = &self.tabs[i] {
            if t.read(cx).is_dirty(cx) {
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
    pub fn database_count(&self) -> usize {
        self.databases.len()
    }

    pub fn has_unsaved(&self, cx: &App) -> bool {
        self.tabs
            .iter()
            .any(|t| matches!(t, TabItem::Table(e) if e.read(cx).is_dirty(cx)))
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
                    TabItem::Query(e) => {
                        (e.read(cx).title.clone(), IconName::SquareTerminal, false)
                    }
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
                div()
                    .text_sm()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child(cfg.display_name()),
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
