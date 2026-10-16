use crate::message::Message;
use crate::model::ConnectionDialog;
use db_core::connection::DriverKind;
use iced::widget::{button, column, container, pick_list, row, text, text_input};
use iced::{Element, Length};

const DRIVERS: [DriverKind; 3] = [DriverKind::Postgres, DriverKind::MySql, DriverKind::MsSql];

pub fn view(dialog: &ConnectionDialog) -> Element<'_, Message> {
    let title = if dialog.editing_id.is_some() { "Edit Connection" } else { "New Connection" };

    let mut form = column![text(title).size(20)].spacing(10).padding(20).width(Length::Fixed(420.0));

    form = form.push(labeled("Name", text_input("My Database", &dialog.name).on_input(Message::DialogNameChanged)));
    form = form.push(labeled(
        "Driver",
        pick_list(DRIVERS, Some(dialog.driver), Message::DialogDriverChanged).width(Length::Fill),
    ));
    form = form.push(labeled("Host", text_input("localhost", &dialog.host).on_input(Message::DialogHostChanged)));
    form = form.push(labeled("Port", text_input("5432", &dialog.port).on_input(Message::DialogPortChanged)));
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
        let message = match result {
            Ok(info) => format!("Connected: {info}"),
            Err(e) => format!("Failed: {e}"),
        };
        form = form.push(text(message).size(13));
    } else if dialog.testing {
        form = form.push(text("Testing connection…").size(13));
    }

    form = form.push(
        row![
            button(text("Test Connection").size(13)).style(button::secondary).on_press(Message::TestConnection),
            button(text("Cancel").size(13)).style(button::text).on_press(Message::CancelDialog),
            iced::widget::Space::new().width(Length::Fill),
            button(text("Save Connection").size(13)).style(button::primary).on_press(Message::SaveConnection),
        ]
        .spacing(10),
    );

    container(form).padding(10).style(container::rounded_box).into()
}

fn labeled<'a>(label: &'a str, field: impl Into<Element<'a, Message>>) -> Element<'a, Message> {
    column![text(label).size(13), field.into()].spacing(4).into()
}
