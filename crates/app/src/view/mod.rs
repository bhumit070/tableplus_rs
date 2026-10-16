mod connection_dialog;
mod grid;
mod query_tab;
mod sidebar;

use crate::message::Message;
use crate::model::App;
use iced::widget::{column, container, row, text, Space, Stack};
use iced::{Element, Length};

pub fn view(state: &App) -> Element<'_, Message> {
    let body = row![sidebar::view(state), query_tab::view(state)].height(Length::Fill);

    let mut layout = column![body].height(Length::Fill);
    if let Some(status) = &state.status {
        layout = layout.push(status_bar(status));
    }

    let content: Element<'_, Message> = layout.into();

    match &state.dialog {
        Some(dialog) => {
            let modal = container(connection_dialog::view(dialog))
                .width(Length::Fill)
                .height(Length::Fill)
                .center(Length::Fill)
                .style(|_theme| container::Style { background: Some(iced::Color::BLACK.scale_alpha(0.6).into()), ..Default::default() });
            Stack::with_children(vec![content, modal.into()]).into()
        }
        None => content,
    }
}

fn status_bar(status: &str) -> Element<'_, Message> {
    container(row![text(status.to_string()), Space::new().width(Length::Fill), iced::widget::button("Dismiss").on_press(Message::StatusDismissed)].spacing(10))
        .padding(8)
        .width(Length::Fill)
        .into()
}
