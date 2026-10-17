mod connect;
mod editor;
mod message;
mod model;
mod store;
mod theme;
mod update;
mod view;

use model::App;

fn main() -> iced::Result {
    tracing_subscriber::fmt::try_init().ok();

    iced::application(App::boot, update::update, view::view)
        .title(App::title)
        .theme(App::theme)
        .run()
}
