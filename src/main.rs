mod csv_import;
mod db;
mod export;
mod history;
mod import;
mod rncryptor;
mod runtime;
mod saved;
mod settings;
mod store;
mod ui;

use gpui::{App, Bounds, Menu, MenuItem, WindowBounds, WindowOptions, prelude::*, px, size};
use gpui_component::{Root, Theme, TitleBar};

use ui::app::AppView;

fn main() {
    gpui_platform::application()
        .with_assets(gpui_kit_assets::AllAssets)
        .run(|cx: &mut App| {
            gpui_component::init(cx);
            Theme::sync_system_appearance(None, cx);
            settings::init(cx);
            cx.bind_keys(ui::key_bindings());
            cx.on_action(|_: &ui::Quit, cx| cx.quit());
            ui::app::register_actions(cx);
            cx.on_action(|_: &ui::ZoomIn, cx| ui::app::zoom(1., cx));
            cx.on_action(|_: &ui::ZoomOut, cx| ui::app::zoom(-1., cx));
            cx.set_menus(vec![Menu {
                name: "Tabula".into(),
                items: vec![MenuItem::action("Quit Tabula", ui::Quit)],
                disabled: false,
            }]);

            let bounds = Bounds::centered(None, size(px(1360.), px(860.)), cx);
            cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    ..TitleBar::window_options()
                },
                |window, cx| {
                    let view = cx.new(|cx| AppView::new(window, cx));
                    cx.new(|cx| Root::new(view, window, cx))
                },
            )
            .expect("open window");
            cx.activate(true);
        });
}
