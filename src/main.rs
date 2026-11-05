mod csv_import;
mod db;
mod export;
mod history;
mod import;
mod rncryptor;
mod restore;
mod runtime;
mod saved;
mod settings;
mod store;
mod ui;

use gpui::{App, Bounds, Menu, MenuItem, WindowBounds, WindowOptions, prelude::*, px, size};
use gpui_component::{Root, Theme, TitleBar};

use ui::app::AppView;

/// The menu bar: every command, with its shortcut shown, so features
/// can be found without knowing the keys.
fn menus() -> Vec<Menu> {
    use MenuItem as I;
    let menu = |name: &str, items: Vec<MenuItem>| Menu {
        name: name.to_string().into(),
        items,
        disabled: false,
    };
    vec![
        menu(
            "Tabula",
            vec![
                I::action("Settings…", ui::OpenSettings),
                I::separator(),
                I::action("Quit Tabula", ui::Quit),
            ],
        ),
        menu(
            "Connection",
            vec![
                I::action("Open Connection…", ui::OpenConnection),
                I::action("Switch Database…", ui::SwitchDatabase),
                I::action("New Database…", ui::NewDatabase),
                I::action("Drop Database…", ui::DropDatabase),
                I::action("Connections Home", ui::GoHome),
                I::separator(),
                I::action("Refresh", ui::Refresh),
                I::action("Processes…", ui::ShowProcesses),
                I::separator(),
                I::action("Backup…", ui::BackupDatabase),
                I::action("Restore…", ui::RestoreDatabase),
            ],
        ),
        menu(
            "Query",
            vec![
                I::action("New Query Tab", ui::NewQuery),
                I::action("Run", ui::RunQuery),
                I::action("Cancel Running Query", ui::CancelQuery),
                I::action("Format SQL", ui::FormatSql),
                I::action("Explain", ui::ExplainQuery),
                I::separator(),
                I::action("Save Query…", ui::SaveQuery),
                I::action("Saved Queries…", ui::SavedQueries),
                I::action("Query History…", ui::QueryHistory),
            ],
        ),
        menu(
            "Data",
            vec![
                I::action("Commit Changes", ui::Commit),
                I::action("Discard Changes", ui::Discard),
                I::separator(),
                I::action("Add Row", ui::AddRow),
                I::action("Delete Selected Rows", ui::DeleteRows),
                I::action("Set NULL", ui::SetNull),
                I::separator(),
                I::action("Filter", ui::ToggleFilter),
                I::action("Export…", ui::ExportData),
            ],
        ),
        menu(
            "View",
            vec![
                I::action("Row Detail", ui::ToggleRowDetail),
                I::action("Toggle Light / Dark", ui::ToggleTheme),
                I::separator(),
                I::action("Zoom In", ui::ZoomIn),
                I::action("Zoom Out", ui::ZoomOut),
                I::separator(),
                I::action("Next Tab", ui::NextTab),
                I::action("Previous Tab", ui::PrevTab),
                I::action("Close Tab", ui::CloseTab),
            ],
        ),
    ]
}

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
            cx.set_menus(menus());

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
