use super::components;
use crate::message::Message;
use crate::model::App;
use db_core::catalog::TableKind;
use db_core::connection::ConnectionConfig;
use iced::widget::{button, column, container, row, scrollable, text, Space};
use iced::{Alignment, Element, Length, Padding};

const SIDEBAR_WIDTH: f32 = 300.0;

pub fn view(state: &App) -> Element<'_, Message> {
    let mut list = column![].spacing(12).width(Length::Fill).padding([4, 0]);

    list = list.push(
        row![
            components::section_header("Connections"),
            Space::new().width(Length::Fill),
            button(text("+ New").size(12)).padding([4, 10]).on_press(Message::ShowNewConnectionDialog),
        ]
        .align_y(Alignment::Center),
    );
    list = list.push(
        row![
            button(text("Export").size(11)).style(button::text).padding(0).on_press(Message::ExportConnections),
            text("·").size(11).color(crate::theme::MUTED_TEXT),
            button(text("Import").size(11)).style(button::text).padding(0).on_press(Message::ImportConnections),
        ]
        .spacing(6)
        .align_y(Alignment::Center),
    );

    if state.connections.is_empty() {
        list = list.push(text("No saved connections yet.").size(12).color(crate::theme::MUTED_TEXT));
    }

    for connection in &state.connections {
        list = list.push(connection_card(state, connection));
    }

    container(scrollable(list.spacing(10)).width(Length::Fill))
        .width(Length::Fixed(SIDEBAR_WIDTH))
        .height(Length::Fill)
        .padding(14)
        .into()
}

fn connection_card<'a>(state: &'a App, connection: &'a ConnectionConfig) -> Element<'a, Message> {
    let session = state.sessions.get(&connection.id);
    let is_expanded = state.expanded_connections.contains(&connection.id);
    let is_connected = session.and_then(|s| s.adapter.clone()).is_some();
    let is_connecting = session.map(|s| s.connecting).unwrap_or(false);

    let header = button(
        row![
            components::driver_badge(connection.driver),
            column![
                text(connection.name.clone()).size(14),
                text(connection.driver.label()).size(11).color(crate::theme::MUTED_TEXT),
            ]
            .spacing(1),
            Space::new().width(Length::Fill),
            components::status_dot(is_connecting, is_connected),
        ]
        .spacing(10)
        .align_y(Alignment::Center),
    )
    .on_press(if is_connected { Message::ToggleSchemaTree(connection.id) } else { Message::Connect(connection.id) })
    .width(Length::Fill)
    .padding([8, 10])
    .style(button::text);

    let mut card = column![header].spacing(6);

    if is_expanded {
        let mut actions = row![
            action_button("Query", Message::NewTab(Some(connection.id))),
            action_button("Edit", Message::EditConnection(connection.id)),
        ]
        .spacing(10);
        if is_connected {
            actions = actions.push(action_button("Disconnect", Message::Disconnect(connection.id)));
        }
        actions = actions.push(action_button("Delete", Message::DeleteConnection(connection.id)));

        card = card.push(container(actions).padding(Padding { top: 0.0, right: 0.0, bottom: 4.0, left: 44.0 }));

        if let Some(session) = session {
            if is_connecting {
                card = card.push(indented(text("Connecting…").size(12).color(crate::theme::MUTED_TEXT)));
            }
            if let Some(error) = &session.error {
                card = card.push(indented(
                    container(text(error.clone()).size(12)).padding(8).style(components::error_banner).width(Length::Fill),
                ));
            }
            if let Some(catalog) = &session.catalog {
                card = card.push(schema_tree(connection.id, catalog));
            }
        }
    }

    container(card)
        .padding(6)
        .style(if is_expanded { components::card_active } else { components::card })
        .into()
}

fn action_button(label: &str, message: Message) -> Element<'_, Message> {
    button(text(label).size(11)).style(button::text).padding([3, 8]).on_press(message).into()
}

fn indented<'a>(content: impl Into<Element<'a, Message>>) -> Element<'a, Message> {
    container(content).padding(Padding { top: 0.0, right: 10.0, bottom: 0.0, left: 44.0 }).into()
}

fn schema_tree(connection_id: uuid::Uuid, catalog: &db_core::catalog::Catalog) -> Element<'_, Message> {
    let mut tree = column![].spacing(4).padding(Padding { top: 4.0, right: 4.0, bottom: 4.0, left: 44.0 });

    if catalog.databases.iter().all(|d| d.schemas.iter().all(|s| s.tables.is_empty())) {
        tree = tree.push(text("No tables found.").size(12).color(crate::theme::MUTED_TEXT));
        return tree.into();
    }

    for database in &catalog.databases {
        for schema in &database.schemas {
            if schema.tables.is_empty() {
                continue;
            }
            tree = tree.push(text(schema.name.clone()).size(11).color(crate::theme::MUTED_TEXT));
            for table in &schema.tables {
                let icon = match table.kind {
                    TableKind::Table => "▤",
                    TableKind::View => "◫",
                    TableKind::MaterializedView => "◧",
                };
                tree = tree.push(
                    row![
                        text(format!("{icon}  {}", table.name)).size(12).width(Length::Fill),
                        button(text("Import").size(10))
                            .style(button::text)
                            .padding([1, 4])
                            .on_press(Message::ImportIntoTableRequested(connection_id, table.name.clone())),
                    ]
                    .spacing(4)
                    .align_y(Alignment::Center),
                );
            }
        }
    }

    tree.into()
}
