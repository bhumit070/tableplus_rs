//! Screenshots of the real app, rendered offscreen with the GPU renderer, for
//! checking the look of each screen in both themes. Not part of the normal
//! test run (needs a GPU and takes a while):
//!     cargo test screenshots -- --ignored --nocapture
//! PNGs land in target/shots/. The data is synthetic (a temp SQLite file).

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{AnyWindowHandle, HeadlessAppContext, Window, px, size};
use gpui_component::{Root, Theme, ThemeMode};

use super::*;
use crate::db::{ConnectionConfig, Driver};
use chrono::Utc;
use futures::executor::block_on;
use gpui_component::init;
use gpui_kit_assets::AllAssets;
use gpui_platform::current_headless_renderer;
use uuid::Uuid;

const W: f32 = 1512.;
const H: f32 = 944.;

struct Shots {
    cx: HeadlessAppContext,
    window: AnyWindowHandle,
    app: Entity<AppView>,
    dir: PathBuf,
}

impl Shots {
    fn new() -> Self {
        // The platform's own text system needs the main thread, which tests
        // don't run on; Core Text itself doesn't.
        let mut cx = HeadlessAppContext::with_platform(
            Arc::new(gpui_apple::AppleTextSystem::new()),
            Arc::new(AllAssets),
            current_headless_renderer,
        );
        cx.allow_parking();
        cx.update(|cx| {
            init(cx);
            crate::ui::theme::init(cx);
            crate::settings::init(cx);
            cx.bind_keys(crate::ui::key_bindings());
            register_actions(cx);
            Theme::change(ThemeMode::Dark, None, cx);
        });
        let mut app = None;
        let window = cx
            .open_window(size(px(W), px(H)), |window, cx| {
                let view = cx.new(|cx| AppView::new(window, cx));
                app = Some(view.clone());
                cx.new(|cx| Root::new(view, window, cx))
            })
            .unwrap();
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/shots");
        std::fs::create_dir_all(&dir).unwrap();
        Self {
            cx,
            window: window.into(),
            app: app.unwrap(),
            dir,
        }
    }

    fn settle(&mut self) {
        // Let entrance animations (dialogs) finish.
        self.cx.advance_clock(Duration::from_secs(1));
        for _ in 0..5 {
            self.cx.run_until_parked();
            std::thread::sleep(Duration::from_millis(30));
        }
        // Draw a fresh frame.
        self.cx
            .update_window(self.window, |_, window, _| window.refresh())
            .unwrap();
        self.cx.run_until_parked();
    }

    fn wait(&mut self, what: &str, mut done: impl FnMut(&mut App) -> bool) {
        let start = Instant::now();
        while !self.cx.update(&mut done) {
            assert!(
                start.elapsed() < Duration::from_secs(15),
                "timed out: {what}"
            );
            std::thread::sleep(Duration::from_millis(20));
            self.cx.run_until_parked();
        }
    }

    fn in_window<R>(&mut self, f: impl FnOnce(&mut Window, &mut App) -> R) -> R {
        self.cx
            .update_window(self.window, |_, window, cx| f(window, cx))
            .unwrap()
    }

    /// Saves `name-dark.png` and `name-light.png`.
    fn snap(&mut self, name: &str) {
        for mode in [ThemeMode::Dark, ThemeMode::Light] {
            self.cx.update(|cx| {
                Theme::change(mode, None, cx);
                crate::settings::apply_theme(cx);
            });
            self.settle();
            let image = self.cx.capture_screenshot(self.window).unwrap();
            let suffix = if mode.is_dark() { "dark" } else { "light" };
            let path = self.dir.join(format!("{name}-{suffix}.png"));
            image.save(&path).unwrap();
            println!("{}", path.display());
        }
        self.cx
            .update(|cx| Theme::change(ThemeMode::Dark, None, cx));
    }
}

/// A SQLite file with a small shop: customers and orders (synthetic data).
fn demo_db() -> ConnectionConfig {
    let mut cfg = ConnectionConfig::new(Driver::Sqlite);
    cfg.name = "shop-demo".into();
    let file = std::env::temp_dir().join(format!("tabula-shots-{}.db", cfg.id));
    std::fs::write(&file, b"").unwrap();
    cfg.file = file.to_string_lossy().into();
    let setup = cfg.clone();
    block_on(crate::runtime::spawn(async move {
        let c = crate::db::Connection::connect(&setup).await?;
        let mut sql = String::from(
            "create table customers (id integer primary key, name varchar(40) not null, \
               email text, country char(2), vip boolean, external_id uuid); \
             create table orders (id integer primary key, \
               customer_id integer references customers(id), \
               status text check (status in ('pending','paid','shipped','refunded')), \
               total numeric(12,2), currency text, created_at text, metadata text); \
             create table invoices (id integer primary key, order_id integer); \
             create table products (id integer primary key, name text); \
             create view active_customers as select * from customers;",
        );
        let names = ["Priya Raman", "Marcus Lee", "Ana Souza", "Kenji Watanabe", "Amara Okafor"];
        for (i, n) in names.iter().enumerate() {
            let email = n.to_lowercase().replace(' ', ".") + "@example.com";
            sql += &format!(
                "insert into customers values ({}, '{n}', '{email}', '{}', {}, '{}');",
                i + 1,
                ["IN", "US", "BR", "JP", "NG"][i],
                if i % 2 == 0 { "true" } else { "false" },
                Uuid::new_v4()
            );
        }
        let status = ["pending", "paid", "shipped", "refunded", "pending", "paid"];
        let cur = ["USD", "EUR", "GBP", "USD", "INR"];
        for i in 0..40u32 {
            let meta = if i % 4 == 3 {
                "null".to_string()
            } else {
                format!("'{{\"source\":\"{}\",\"items\":{}}}'", ["web", "ios"][(i % 2) as usize], i % 5 + 1)
            };
            sql += &format!(
                "insert into orders values ({}, {}, '{}', {}.{:02}, '{}', '2026-11-{:02} {:02}:{:02}:00', {meta});",
                10400 - i * 7,
                i % 5 + 1,
                status[(i % 6) as usize],
                100 + (i * 137) % 1900,
                (i * 37) % 100,
                cur[(i % 5) as usize],
                6 - (i / 12),
                23 - (i % 24),
                (i * 13) % 60
            );
        }
        c.execute(&sql).await?;
        c.close().await;
        Ok(())
    }))
    .unwrap();
    cfg
}

#[test]
#[ignore = "renders with the GPU; run explicitly"]
fn screenshots() {
    let mut s = Shots::new();
    s.settle();
    s.snap("03-home-empty");

    // Settings and the new-connection form, over the empty home.
    let settings = s.in_window(|w, cx| crate::ui::settings_view::SettingsView::open(w, cx));
    s.snap("21-settings");
    s.in_window(|_, cx| settings.update(cx, |v, cx| v.show_shortcuts(cx)));
    s.snap("21-settings-shortcuts");
    s.in_window(|w, cx| w.close_dialog(cx));
    let home = s.cx.update(|cx| s.app.read(cx).home.clone());
    s.in_window(|w, cx| home.update(cx, |h, cx| h.open_form(None, w, cx)));
    s.snap("04-new-connection");
    s.in_window(|w, cx| w.close_dialog(cx));
    // Editing: production, with an SSH tunnel.
    let mut edit = crate::db::ConnectionConfig::new(crate::db::Driver::Postgres);
    edit.name = "acme-production".into();
    edit.host = "10.0.4.12".into();
    edit.user = "app".into();
    edit.database = "shop".into();
    edit.tag = crate::db::EnvTag::Production;
    edit.ssl = true;
    edit.ssh = Some(crate::db::ssh::SshConfig {
        host: "bastion.acme.dev".into(),
        port: 22,
        user: "deploy".into(),
        ..Default::default()
    });
    s.in_window(|w, cx| home.update(cx, |h, cx| h.open_form(Some(edit), w, cx)));
    s.snap("04-edit-connection");
    s.in_window(|w, cx| w.close_dialog(cx));

    let cfg = demo_db();
    let app = s.app.clone();
    s.in_window(|w, cx| app.update(cx, |a, cx| a.connect(cfg, w, cx, |_, _, _| {})));
    s.wait("connection", |cx| app.read(cx).sessions.len() == 1);
    let session = s.cx.update(|cx| app.read(cx).sessions[0].clone());
    s.wait("tables", |cx| {
        session.read(cx).object_index("orders").is_some()
    });
    s.in_window(|w, cx| {
        session.update(cx, |s, cx| {
            let i = s.object_index("orders").unwrap();
            s.open_object(i, w, cx)
        })
    });
    let tab = s.cx.update(|cx| session.read(cx).active_table().unwrap());
    s.wait("rows", |cx| tab.read(cx).loaded_rows(cx) > 0);
    s.snap("01-table");
    s.in_window(|_, cx| tab.update(cx, |t, cx| t.pretend_loading(cx)));
    s.snap("22-loading");
    s.in_window(|_, cx| tab.update(cx, |t, cx| t.show_no_matches(cx)));
    s.snap("22-no-matches");
    s.in_window(|w, cx| tab.update(cx, |t, cx| t.clear_filters(w, cx)));
    s.wait("rows again", |cx| tab.read(cx).loaded_rows(cx) > 0);

    s.in_window(|w, cx| w.dispatch_action(Box::new(crate::ui::ExportData), cx));
    s.snap("12-export");
    s.in_window(|w, cx| w.close_dialog(cx));

    // Staged changes: an edited cell, a deleted row, a new row.
    s.in_window(|w, cx| {
        tab.update(cx, |t, cx| {
            t.stage_cell(37, 2, "refunded", cx);
            t.select_cell(38, 0, w, cx);
            t.delete_rows(&crate::ui::DeleteRows, w, cx);
            t.add_row(&crate::ui::AddRow, w, cx);
        })
    });
    s.snap("01-table-staged");

    // The safe-mode confirmation, as on a production connection.
    let mut prod = s.cx.update(|cx| session.read(cx).cfg().clone());
    prod.tag = crate::db::EnvTag::Production;
    prod.name = "acme-production".into();
    s.in_window(|w, cx| {
        crate::ui::confirm::ConfirmWrite::open(
            &prod,
            "Commit",
            "UPDATE orders SET status = 'refunded' WHERE id = 10393;\n\
             DELETE FROM orders WHERE id = 10386;\n\
             INSERT INTO orders (customer_id, status, total, currency)\n  VALUES (2, 'pending', 89.00, 'USD');"
                .into(),
            w,
            cx,
            std::rc::Rc::new(|_, _| {}),
        )
    });
    s.snap("11-confirm");
    s.in_window(|w, cx| w.close_dialog(cx));

    // Cell editors on customers: a too-long value, then the boolean dropdown.
    s.in_window(|w, cx| {
        session.update(cx, |s, cx| {
            let i = s.object_index("customers").unwrap();
            s.open_object(i, w, cx)
        })
    });
    let customers = s.cx.update(|cx| session.read(cx).active_table().unwrap());
    s.wait("customers", |cx| customers.read(cx).loaded_rows(cx) > 0);
    s.in_window(|w, cx| {
        customers.update(cx, |t, cx| {
            t.select_cell(1, 1, w, cx);
            let long = "Marcus Alexander Lee-Whitfield the Third".to_string() + " Jr.";
            crate::ui::cell_edit::start_edit(
                t,
                1,
                1,
                crate::ui::cell_edit::EditStart::Typed(long),
                w,
                cx,
            );
        })
    });
    s.snap("07-editor-error");
    s.in_window(|w, cx| {
        customers.update(cx, |t, cx| {
            crate::ui::cell_edit::cancel_edit(t, w, cx);
            t.select_cell(0, 4, w, cx);
            crate::ui::cell_edit::start_edit(t, 0, 4, crate::ui::cell_edit::EditStart::Keep, w, cx);
        })
    });
    s.snap("07-editor-bool");
    s.in_window(|w, cx| customers.update(cx, |t, cx| crate::ui::cell_edit::cancel_edit(t, w, cx)));

    s.in_window(|w, cx| session.update(cx, |s, cx| s.new_query(w, cx)));
    s.snap("02-query");
    // Three statements; the last one fails and gets its own tab.
    let query = s.cx.update(|cx| session.read(cx).active_query().unwrap());
    s.in_window(|w, cx| {
        query.update(cx, |q, cx| {
            q.type_and_run(
                "select id, status, total from orders where total > 500 order by total desc;\nupdate orders set currency = currency where id < 0;\nselect nope from orders;",
                w,
                cx,
            )
        })
    });
    s.wait("query", |cx| !query.read(cx).running());
    s.snap("02-query-results");
    s.in_window(|w, cx| {
        query.update(cx, |q, cx| {
            q.type_and_run(
                "select status, count(*) from orders where total > 500 group by status;",
                w,
                cx,
            )
        })
    });
    s.wait("query", |cx| !query.read(cx).running());
    s.in_window(|w, cx| query.update(cx, |q, cx| q.explain(false, w, cx)));
    s.wait("plan", |cx| !query.read(cx).running());
    s.snap("14-explain");
    s.in_window(|w, cx| session.update(cx, |s, cx| s.open_goto(w, cx)));
    s.snap("05-palette");
    s.in_window(|w, cx| w.close_dialog(cx));
    s.settle();
    let app = s.app.clone();
    s.in_window(|w, cx| app.update(cx, |a, cx| a.open_connection_picker(w, cx)));
    s.snap("17-open-connection");
    s.in_window(|w, cx| w.close_dialog(cx));
    s.settle();
    let conn_id = s.cx.update(|cx| session.read(cx).cfg().id.clone());
    let weak = session.downgrade();
    let history = s.in_window(|w, cx| {
        crate::ui::history_view::HistoryView::open(
            weak,
            conn_id,
            crate::ui::history_view::Tab::History,
            w,
            cx,
        )
    });
    s.wait("history", |cx| history.read(cx).loaded());
    s.snap("15-history");
    s.in_window(|w, cx| w.close_dialog(cx));

    // Home with saved connections (not written anywhere); the open demo
    // database is one of them, so "Open now" shows too.
    let demo = s.cx.update(|cx| session.read(cx).cfg().clone());
    s.cx.update(|cx| {
        home.update(cx, |h, cx| {
            h.connections.push(demo);
            for (name, driver, tag, host) in [
                (
                    "acme-production",
                    Driver::Postgres,
                    crate::db::EnvTag::Production,
                    "db.acme.internal",
                ),
                (
                    "acme-staging",
                    Driver::Postgres,
                    crate::db::EnvTag::Staging,
                    "staging-db.acme.internal",
                ),
                (
                    "local-dev",
                    Driver::MySql,
                    crate::db::EnvTag::Local,
                    "localhost",
                ),
                (
                    "redis-cache",
                    Driver::Redis,
                    crate::db::EnvTag::Development,
                    "cache.dev.acme.internal",
                ),
                (
                    "events",
                    Driver::Mongo,
                    crate::db::EnvTag::Local,
                    "localhost",
                ),
            ] {
                let mut c = ConnectionConfig::new(driver);
                c.name = name.into();
                c.tag = tag;
                c.host = host.into();
                if name == "acme-staging" {
                    h.set_failure(
                        &c.id,
                        "connection refused (os error 61): is the server running on staging-db.acme.internal:5432?",
                    );
                }
                if name == "acme-production" {
                    c.ssh = Some(Default::default());
                    c.ssl = true;
                    c.last_used = Some(Utc::now().timestamp() - 3 * 3600);
                }
                h.connections.push(c);
            }
            cx.notify();
        })
    });
    s.cx.update(|cx| {
        s.app.update(cx, |a, cx| {
            a.active = None;
            cx.notify();
        })
    });
    s.snap("03-home");
    // Welcome back: last session on offer.
    s.cx.update(|cx| {
        home.update(cx, |h, cx| {
            let sessions = h
                .connections
                .iter()
                .take(2)
                .map(|c| crate::restore::SessionState {
                    conn_id: c.id.clone(),
                    tabs: vec![],
                    active_tab: 0,
                })
                .collect();
            h.offer_reopen(
                crate::restore::State {
                    sessions,
                    active: None,
                },
                cx,
            );
        })
    });
    s.snap("24-welcome-back");
}
