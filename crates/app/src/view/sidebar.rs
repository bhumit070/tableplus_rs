use crate::message::Message;
use crate::model::App;
use db_core::catalog::TableKind;
use db_core::connection::ConnectionConfig;
use iced::widget::{button, column, container, row, scrollable, text, Space};
use iced::{Element, Length, Padding};

const SIDEBAR_WIDTH: f32 = 300.0;

pub fn view(state: &App) -> Element<'_, Message> {
    let mut list = column![].spacing(10).width(Length::Fill);

    list = list.push(text("Connections").size(16));
    list = list.push(
        row![
            button(text("+ New").size(13)).on_press(Message::ShowNewConnectionDialog),
            Space::new().width(Length::Fill),
            button(text("Export").size(12)).style(button::text).on_press(Message::ExportConnections),
            button(text("Import").size(12)).style(button::text).on_press(Message::ImportConnections),
        ]
        .spacing(6),
    );

    if state.connections.is_empty() {
        list = list.push(text("No saved connections yet.").size(12));
    }

    for connection in &state.connections {
        list = list.push(connection_card(state, connection));
    }

    container(scrollable(list).width(Length::Fill))
        .width(Length::Fixed(SIDEBAR_WIDTH))
        .height(Length::Fill)
        .padding(12)
        .style(container::bordered_box)
        .into()
}

fn connection_card<'a>(state: &'a App, connection: &'a ConnectionConfig) -> Element<'a, Message> {
    let session = state.sessions.get(&connection.id);
    let is_expanded = state.expanded_connections.contains(&connection.id);
    let is_connected = session.and_then(|s| s.adapter.clone()).is_some();
    let is_connecting = session.map(|s| s.connecting).unwrap_or(false);

    let status_glyph = if is_connecting { "◐" } else if is_connected { "●" } else { "○" };

    let name_button = button(
        row![
            text(status_glyph).size(12),
            text(connection.name.clone()).size(14),
            Space::new().width(Length::Fill),
            text(connection.driver.label()).size(11),
        ]
        .spacing(6)
        .align_y(iced::Alignment::Center),
    )
    .on_press(if is_connected { Message::ToggleSchemaTree(connection.id) } else { Message::Connect(connection.id) })
    .width(Length::Fill)
    .style(button::text);

    let mut card = column![name_button].spacing(6);

    if is_expanded {
        let mut actions = row![
            action_button("Query", Message::NewTab(Some(connection.id))),
            action_button("Edit", Message::EditConnection(connection.id)),
        ]
        .spacing(8);
        if is_connected {
            actions = actions.push(action_button("Disconnect", Message::Disconnect(connection.id)));
        }
        actions = actions.push(action_button("Delete", Message::DeleteConnection(connection.id)));

        card = card.push(container(actions).padding(Padding { top: 0.0, right: 0.0, bottom: 0.0, left: 20.0 }));

        if let Some(session) = session {
            if is_connecting {
                card = card.push(indented(text("Connecting…").size(12)));
            }
            if let Some(error) = &session.error {
                card = card.push(indented(text(error.clone()).size(12)));
            }
            if let Some(catalog) = &session.catalog {
                card = card.push(schema_tree(connection.id, catalog));
            }
        }
    }

    container(card).padding(8).style(container::rounded_box).into()
}

fn action_button(label: &str, message: Message) -> Element<'_, Message> {
    button(text(label).size(11)).style(button::text).padding([2, 6]).on_press(message).into()
}

fn indented<'a>(content: impl Into<Element<'a, Message>>) -> Element<'a, Message> {
    container(content).padding(Padding { top: 0.0, right: 0.0, bottom: 0.0, left: 20.0 }).into()
}

fn schema_tree(connection_id: uuid::Uuid, catalog: &db_core::catalog::Catalog) -> Element<'_, Message> {
    let mut tree = column![].spacing(3).padding(Padding { top: 4.0, right: 0.0, bottom: 0.0, left: 20.0 });

    if catalog.databases.iter().all(|d| d.schemas.iter().all(|s| s.tables.is_empty())) {
        tree = tree.push(text("No tables found.").size(12));
        return tree.into();
    }

    for database in &catalog.databases {
        for schema in &database.schemas {
            if schema.tables.is_empty() {
                continue;
            }
            tree = tree.push(text(schema.name.clone()).size(12));
            for table in &schema.tables {
                let icon = match table.kind {
                    TableKind::Table => "▤",
                    TableKind::View => "◫",
                    TableKind::MaterializedView => "◧",
                };
                tree = tree.push(
                    container(
                        row![
                            text(format!("{icon} {}", table.name)).size(12).width(Length::Fill),
                            button(text("Import").size(10))
                                .style(button::text)
                                .padding([1, 4])
                                .on_press(Message::ImportIntoTableRequested(connection_id, table.name.clone())),
                        ]
                        .spacing(4)
                        .align_y(iced::Alignment::Center),
                    )
                    .padding(Padding { top: 0.0, right: 0.0, bottom: 0.0, left: 12.0 }),
                );
            }
        }
    }

    tree.into()
}
