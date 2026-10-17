mod components;
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
    let content = row![
        text(status.to_string()).size(13),
        Space::new().width(Length::Fill),
        iced::widget::button(text("Dismiss").size(12)).style(iced::widget::button::text).on_press(Message::StatusDismissed),
    ]
    .spacing(10)
    .align_y(iced::Alignment::Center);

    container(content)
        .padding([8, 14])
        .width(Length::Fill)
        .style(|_theme| container::Style {
            background: Some(crate::theme::SURFACE_RAISED.into()),
            ..container::Style::default()
        })
        .into()
}
