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
            Arc::new(gpui_kit_assets::AllAssets),
            gpui_platform::current_headless_renderer,
        );
        cx.allow_parking();
        cx.update(|cx| {
            gpui_component::init(cx);
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
    futures::executor::block_on(crate::runtime::spawn(async move {
        let c = crate::db::Connection::connect(&setup).await?;
        let mut sql = String::from(
            "create table customers (id integer primary key, name text not null, \
               email text, country text); \
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
                "insert into customers values ({}, '{n}', '{email}', '{}');",
                i + 1,
                ["IN", "US", "BR", "JP", "NG"][i]
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

    s.in_window(|w, cx| session.update(cx, |s, cx| s.new_query(w, cx)));
    s.snap("02-query");
}
