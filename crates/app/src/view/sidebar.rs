use crate::message::Message;
use crate::model::App;
use db_core::catalog::TableKind;
use db_core::connection::ConnectionConfig;
use iced::widget::{button, column, container, row, scrollable, text, Space};
use iced::{Element, Length};

pub fn view(state: &App) -> Element<'_, Message> {
    let mut list = column![].spacing(4).padding(10);

    list = list.push(
        row![
            button("+ New Connection").on_press(Message::ShowNewConnectionDialog),
            Space::new().width(Length::Fill),
        ]
        .spacing(6),
    );
    list = list.push(
        row![
            button("Export Connections").on_press(Message::ExportConnections),
            button("Import Connections").on_press(Message::ImportConnections),
        ]
        .spacing(6),
    );
    list = list.push(Space::new().height(8));

    for connection in &state.connections {
        list = list.push(connection_row(state, connection));
    }

    container(scrollable(list)).width(Length::Fixed(280.0)).height(Length::Fill).into()
}

fn connection_row<'a>(state: &'a App, connection: &'a ConnectionConfig) -> Element<'a, Message> {
    let session = state.sessions.get(&connection.id);
    let is_expanded = state.expanded_connections.contains(&connection.id);

    let status_label = match session {
        Some(s) if s.connecting => " (connecting…)",
        Some(s) if s.error.is_some() => " (error)",
        Some(s) if s.adapter.is_some() => " (connected)",
        _ => "",
    };

    let mut header = row![
        button(text(format!("{} {}{}", connection.driver.label(), connection.name, status_label)))
            .on_press(if session.and_then(|s| s.adapter.clone()).is_some() {
                Message::ToggleSchemaTree(connection.id)
            } else {
                Message::Connect(connection.id)
            })
            .width(Length::Fill),
        button("Query").on_press(Message::NewTab(Some(connection.id))),
        button("Edit").on_press(Message::EditConnection(connection.id)),
        button("Delete").on_press(Message::DeleteConnection(connection.id)),
    ]
    .spacing(4);

    if session.and_then(|s| s.adapter.clone()).is_some() {
        header = header.push(button("Disconnect").on_press(Message::Disconnect(connection.id)));
    }

    let mut block = column![header].spacing(2);

    if is_expanded {
        if let Some(session) = session {
            if let Some(error) = &session.error {
                block = block.push(text(format!("  {error}")).size(12));
            }
            if let Some(catalog) = &session.catalog {
                block = block.push(schema_tree(connection.id, catalog));
            }
        }
    }

    container(block).padding(4).into()
}

fn schema_tree(connection_id: uuid::Uuid, catalog: &db_core::catalog::Catalog) -> Element<'_, Message> {
    let mut tree = column![].spacing(2).padding(iced::Padding { top: 0.0, right: 0.0, bottom: 0.0, left: 16.0 });

    for database in &catalog.databases {
        for schema in &database.schemas {
            tree = tree.push(text(schema.name.clone()).size(13));
            for table in &schema.tables {
                let icon = match table.kind {
                    TableKind::Table => "▤",
                    TableKind::View => "◫",
                    TableKind::MaterializedView => "◧",
                };
                tree = tree.push(
                    row![
                        text(format!("  {icon} {}", table.name)).size(12).width(Length::Fill),
                        button(text("Import").size(11))
                            .on_press(Message::ImportIntoTableRequested(connection_id, table.name.clone())),
                    ]
                    .spacing(4),
                );
            }
        }
    }

    tree.into()
}
