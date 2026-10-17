use crate::message::Message;
use crate::model::ConnectionDialog;
use db_core::connection::DriverKind;
use iced::widget::{button, column, container, pick_list, row, text, text_input, Space};
use iced::{Border, Element, Length};

const DRIVERS: [DriverKind; 3] = [DriverKind::Postgres, DriverKind::MySql, DriverKind::MsSql];

pub fn view(dialog: &ConnectionDialog) -> Element<'_, Message> {
    let title = if dialog.editing_id.is_some() { "Edit Connection" } else { "New Connection" };

    let mut form = column![text(title).size(18)].spacing(14).padding(22).width(Length::Fixed(440.0));

    form = form.push(labeled("Name", text_input("My Database", &dialog.name).on_input(Message::DialogNameChanged)));
    form = form.push(labeled(
        "Driver",
        pick_list(DRIVERS, Some(dialog.driver), Message::DialogDriverChanged).width(Length::Fill),
    ));
    form = form.push(
        row![
            labeled("Host", text_input("localhost", &dialog.host).on_input(Message::DialogHostChanged)).explicit_width(Length::FillPortion(3)),
            labeled("Port", text_input("5432", &dialog.port).on_input(Message::DialogPortChanged)).explicit_width(Length::FillPortion(1)),
        ]
        .spacing(10),
    );
    form = form.push(labeled("Username", text_input("", &dialog.username).on_input(Message::DialogUsernameChanged)));
    form = form.push(labeled(
        "Password",
        text_input("", &dialog.password).secure(true).on_input(Message::DialogPasswordChanged),
    ));
    form = form.push(labeled(
        "Database",
        text_input("(optional)", &dialog.database).on_input(Message::DialogDatabaseChanged),
    ));

    if let Some(result) = &dialog.test_result {
        let (message, is_ok) = match result {
            Ok(info) => (format!("Connected: {info}"), true),
            Err(e) => (format!("Failed: {e}"), false),
        };
        let color = if is_ok { crate::theme::SUCCESS } else { crate::theme::DANGER };
        form = form.push(text(message).size(13).color(color));
    } else if dialog.testing {
        form = form.push(text("Testing connection…").size(13).color(crate::theme::MUTED_TEXT));
    }

    form = form.push(
        row![
            button(text("Test Connection").size(13)).style(button::secondary).on_press(Message::TestConnection),
            button(text("Cancel").size(13)).style(button::text).on_press(Message::CancelDialog),
            Space::new().width(Length::Fill),
            button(text("Save Connection").size(13)).padding([8, 16]).style(button::primary).on_press(Message::SaveConnection),
        ]
        .spacing(10)
        .align_y(iced::Alignment::Center),
    );

    container(form)
        .style(|_theme| iced::widget::container::Style {
            background: Some(crate::theme::SURFACE.into()),
            border: Border { color: crate::theme::BORDER, width: 1.0, radius: 12.0.into() },
            shadow: iced::Shadow {
                color: iced::Color::BLACK,
                offset: iced::Vector::new(0.0, 8.0),
                blur_radius: 32.0,
            },
            ..iced::widget::container::Style::default()
        })
        .into()
}

struct Labeled<'a> {
    label: &'a str,
    field: Element<'a, Message>,
    width: Length,
}

impl<'a> Labeled<'a> {
    fn explicit_width(mut self, width: Length) -> Self {
        self.width = width;
        self
    }
}

impl<'a> From<Labeled<'a>> for Element<'a, Message> {
    fn from(value: Labeled<'a>) -> Self {
        column![text(value.label).size(12).color(crate::theme::MUTED_TEXT), value.field]
            .spacing(5)
            .width(value.width)
            .into()
    }
}

fn labeled<'a>(label: &'a str, field: impl Into<Element<'a, Message>>) -> Labeled<'a> {
    Labeled { label, field: field.into(), width: Length::Fill }
}
