use crate::message::{DialogIntent, Message};
use crate::model::{App, ConnectionDialog, Session, Tab};
use db_core::adapter::DbAdapter;
use db_core::catalog::ColumnInfo;
use db_core::connection::DriverKind;
use db_core::export::ExportFormat;
use db_core::result::QueryResult;
use iced::widget::text_editor;
use iced::Task;
use std::path::PathBuf;
use std::sync::Arc;

pub fn update(state: &mut App, message: Message) -> Task<Message> {
    match message {
        Message::ShowNewConnectionDialog => {
            state.dialog = Some(ConnectionDialog::new());
            Task::none()
        }
        Message::EditConnection(id) => {
            if let Some(config) = state.find_connection(id) {
                let password = crate::store::secrets::get_password(config).ok().flatten();
                state.dialog = Some(ConnectionDialog::from_config(config, password));
            }
            Task::none()
        }
        Message::CancelDialog => {
            state.dialog = None;
            Task::none()
        }
        Message::DialogNameChanged(v) => with_dialog(state, |d| d.name = v),
        Message::DialogDriverChanged(v) => with_dialog(state, |d| d.driver = v),
        Message::DialogHostChanged(v) => with_dialog(state, |d| d.host = v),
        Message::DialogPortChanged(v) => with_dialog(state, |d| d.port = v),
        Message::DialogUsernameChanged(v) => with_dialog(state, |d| d.username = v),
        Message::DialogPasswordChanged(v) => with_dialog(state, |d| d.password = v),
        Message::DialogDatabaseChanged(v) => with_dialog(state, |d| d.database = v),

        Message::TestConnection => {
            let Some(dialog) = &mut state.dialog else { return Task::none() };
            dialog.testing = true;
            dialog.test_result = None;
            let config = dialog.to_config();
            let password = dialog.password.clone();
            Task::perform(
                async move {
                    crate::connect::connect(config, password).await.map(|a| a.server_info()).map_err(|e| e.to_string())
                },
                Message::TestConnectionFinished,
            )
        }
        Message::TestConnectionFinished(result) => {
            if let Some(dialog) = &mut state.dialog {
                dialog.testing = false;
                dialog.test_result = Some(result);
            }
            Task::none()
        }
        Message::SaveConnection => {
            let Some(dialog) = state.dialog.take() else { return Task::none() };
            let config = dialog.to_config();
            let name = config.name.clone();
            let mut problems = Vec::new();

            // An empty password field means "leave it as-is" (e.g. editing
            // a connection without wanting to touch its saved password),
            // not "erase the saved password" — only write when non-empty.
            if !dialog.password.is_empty() {
                if let Err(e) = crate::store::secrets::set_password(&config, &dialog.password) {
                    problems.push(format!("password: {e}"));
                }
            }

            if let Some(existing) = state.connections.iter_mut().find(|c| c.id == config.id) {
                *existing = config;
            } else {
                state.connections.push(config);
            }
            if let Err(e) = crate::store::connections::save(&state.connections) {
                problems.push(format!("connection list: {e}"));
            }

            state.status = Some(if problems.is_empty() {
                format!("Saved \"{name}\"")
            } else {
                format!("Saved \"{name}\" with problems — {}", problems.join("; "))
            });
            Task::none()
        }
        Message::DeleteConnection(id) => {
            if let Some(config) = state.connections.iter().find(|c| c.id == id).cloned() {
                let _ = crate::store::secrets::delete_password(&config);
            }
            state.connections.retain(|c| c.id != id);
            state.sessions.remove(&id);
            let _ = crate::store::connections::save(&state.connections);
            Task::none()
        }
        Message::ExportConnections => Task::perform(
            async { rfd::AsyncFileDialog::new().set_file_name("connections.json").save_file().await.map(|h| h.path().to_path_buf()) },
            |path| Message::ConnectionsFileChosen(path, DialogIntent::ExportConnections),
        ),
        Message::ImportConnections => Task::perform(
            async {
                rfd::AsyncFileDialog::new().add_filter("JSON", &["json"]).pick_file().await.map(|h| h.path().to_path_buf())
            },
            |path| Message::ConnectionsFileChosen(path, DialogIntent::ImportConnections),
        ),
        Message::ConnectionsFileChosen(None, _) => Task::none(),
        Message::ConnectionsFileChosen(Some(path), DialogIntent::ExportConnections) => {
            match crate::store::connections::export_to(&state.connections, &path) {
                Ok(()) => state.status = Some(format!("Exported connections to {}", path.display())),
                Err(e) => state.status = Some(format!("Export failed: {e}")),
            }
            Task::none()
        }
        Message::ConnectionsFileChosen(Some(path), DialogIntent::ImportConnections) => {
            match crate::store::connections::import_from(&path) {
                Ok(imported) => {
                    let count = imported.len();
                    for config in imported {
                        if let Some(existing) = state.connections.iter_mut().find(|c| c.id == config.id) {
                            *existing = config;
                        } else {
                            state.connections.push(config);
                        }
                    }
                    let _ = crate::store::connections::save(&state.connections);
                    state.status = Some(format!("Imported {count} connection(s)"));
                }
                Err(e) => state.status = Some(format!("Import failed: {e}")),
            }
            Task::none()
        }

        Message::Connect(id) => {
            let Some(config) = state.find_connection(id).cloned() else { return Task::none() };
            let password = crate::store::secrets::get_password(&config).ok().flatten().unwrap_or_default();
            let session = state.sessions.entry(id).or_insert_with(|| Session {
                config: config.clone(),
                adapter: None,
                catalog: None,
                connecting: false,
                error: None,
            });
            session.connecting = true;
            session.error = None;
            Task::perform(crate::connect::connect(config, password), move |result| {
                Message::Connected(id, result.map(Arc::from).map_err(|e| e.to_string()))
            })
        }
        Message::Connected(id, Ok(adapter)) => {
            if let Some(session) = state.sessions.get_mut(&id) {
                session.adapter = Some(adapter.clone());
                session.connecting = false;
                session.error = None;
            }
            if !state.expanded_connections.contains(&id) {
                state.expanded_connections.push(id);
            }
            Task::perform(async move { adapter.fetch_catalog().await }, move |result| {
                Message::CatalogFetched(id, result.map_err(|e| e.to_string()))
            })
        }
        Message::Connected(id, Err(e)) => {
            if let Some(session) = state.sessions.get_mut(&id) {
                session.connecting = false;
                session.error = Some(e.clone());
            }
            // A failed connection used to fail silently unless the user
            // happened to already have that connection's row expanded:
            // auto-expand it (same as a success) and also surface the
            // error as a status toast, so clicking a connection always
            // visibly does *something*.
            if !state.expanded_connections.contains(&id) {
                state.expanded_connections.push(id);
            }
            let name = state.find_connection(id).map(|c| c.name.clone()).unwrap_or_default();
            state.status = Some(format!("Couldn't connect to {name}: {e}"));
            Task::none()
        }
        Message::CatalogFetched(id, Ok(catalog)) => {
            if let Some(session) = state.sessions.get_mut(&id) {
                session.catalog = Some(catalog);
                session.error = None;
            }
            Task::none()
        }
        Message::CatalogFetched(id, Err(e)) => {
            if let Some(session) = state.sessions.get_mut(&id) {
                session.error = Some(e);
            }
            Task::none()
        }
        Message::ToggleSchemaTree(id) => {
            if let Some(pos) = state.expanded_connections.iter().position(|c| *c == id) {
                state.expanded_connections.remove(pos);
            } else {
                state.expanded_connections.push(id);
            }
            Task::none()
        }
        Message::Disconnect(id) => {
            state.sessions.remove(&id);
            Task::none()
        }

        Message::NewTab(connection_id) => {
            state.tabs.push(Tab::new(connection_id));
            state.active_tab = state.tabs.len() - 1;
            Task::none()
        }
        Message::CloseTab(idx) => {
            if state.tabs.len() > 1 {
                if idx < state.tabs.len() {
                    state.tabs.remove(idx);
                }
                if state.active_tab >= state.tabs.len() {
                    state.active_tab = state.tabs.len() - 1;
                }
            } else {
                state.tabs[0] = Tab::new(None);
            }
            Task::none()
        }
        Message::SwitchTab(idx) => {
            if idx < state.tabs.len() {
                state.active_tab = idx;
            }
            Task::none()
        }
        Message::EditorAction(idx, action) => {
            let Some(tab) = state.tabs.get_mut(idx) else { return Task::none() };
            tab.editor.perform(action);

            let text = tab.editor.text();
            let cursor = crate::editor::cursor::byte_offset(&tab.editor);
            let dialect_kind = tab.connection_id.and_then(|cid| state.sessions.get(&cid)).map(|s| s.config.driver);
            let dialect = dialect_kind.map(sql_intel::dialect_for).unwrap_or_else(sql_intel::generic_dialect);
            let catalog = tab.connection_id.and_then(|cid| state.sessions.get(&cid)).and_then(|s| s.catalog.as_ref());

            tab.suggestions = sql_intel::complete(&text, cursor, catalog, dialect.as_ref());
            tab.selected_suggestion = 0;
            Task::none()
        }
        Message::AcceptSuggestion(idx, suggestion_idx) => {
            let Some(tab) = state.tabs.get_mut(idx) else { return Task::none() };
            let Some(suggestion) = tab.suggestions.get(suggestion_idx).cloned() else { return Task::none() };

            let text = tab.editor.text();
            let cursor = crate::editor::cursor::byte_offset(&tab.editor);
            let partial_len = text[..cursor.min(text.len())].chars().rev().take_while(|c| c.is_alphanumeric() || *c == '_').count();

            for _ in 0..partial_len {
                tab.editor.perform(text_editor::Action::Edit(text_editor::Edit::Backspace));
            }
            tab.editor.perform(text_editor::Action::Edit(text_editor::Edit::Paste(Arc::new(suggestion.label))));
            tab.suggestions.clear();
            Task::none()
        }
        Message::DismissSuggestions(idx) => {
            if let Some(tab) = state.tabs.get_mut(idx) {
                tab.suggestions.clear();
            }
            Task::none()
        }
        Message::SelectNextSuggestion(idx) => {
            if let Some(tab) = state.tabs.get_mut(idx) {
                if !tab.suggestions.is_empty() {
                    tab.selected_suggestion = (tab.selected_suggestion + 1) % tab.suggestions.len();
                }
            }
            Task::none()
        }
        Message::SelectPrevSuggestion(idx) => {
            if let Some(tab) = state.tabs.get_mut(idx) {
                let len = tab.suggestions.len();
                if len > 0 {
                    tab.selected_suggestion = (tab.selected_suggestion + len - 1) % len;
                }
            }
            Task::none()
        }

        Message::RunQuery(idx) => {
            let Some(tab) = state.tabs.get_mut(idx) else { return Task::none() };
            let sql = tab.editor.text();
            let Some(cid) = tab.connection_id else {
                tab.error = Some("This tab has no active connection.".to_string());
                return Task::none();
            };
            let Some(adapter) = state.sessions.get(&cid).and_then(|s| s.adapter.clone()) else {
                tab.error = Some("Not connected yet.".to_string());
                return Task::none();
            };
            tab.running = true;
            tab.error = None;
            Task::perform(async move { adapter.execute(&sql).await }, move |result| {
                Message::QueryFinished(idx, result.map_err(|e| e.to_string()))
            })
        }
        Message::QueryFinished(idx, result) => {
            if let Some(tab) = state.tabs.get_mut(idx) {
                tab.running = false;
                match result {
                    Ok(r) => {
                        tab.result = Some(r);
                        tab.error = None;
                    }
                    Err(e) => {
                        tab.error = Some(e);
                        tab.result = None;
                    }
                }
            }
            Task::none()
        }
        Message::CancelQuery(idx) => {
            let Some(tab) = state.tabs.get(idx) else { return Task::none() };
            let Some(cid) = tab.connection_id else { return Task::none() };
            let Some(adapter) = state.sessions.get(&cid).and_then(|s| s.adapter.clone()) else { return Task::none() };
            Task::perform(async move { adapter.cancel().await }, |_| Message::StatusDismissed)
        }

        Message::ExportResultRequested(idx, format) => {
            let Some(tab) = state.tabs.get(idx) else { return Task::none() };
            let Some(result) = tab.result.clone() else {
                state.status = Some("No result to export yet.".to_string());
                return Task::none();
            };
            let driver = tab.connection_id.and_then(|cid| state.sessions.get(&cid)).map(|s| s.config.driver).unwrap_or(DriverKind::Postgres);
            Task::perform(export_result_to_file(result, format, driver), move |res| Message::ExportFinished(idx, res))
        }
        Message::ExportFinished(_idx, result) => {
            state.status = Some(match result {
                Ok(path) => format!("Exported to {}", path.display()),
                Err(e) => format!("Export failed: {e}"),
            });
            Task::none()
        }
        Message::ImportIntoTableRequested(cid, table_name) => {
            let Some(session) = state.sessions.get(&cid) else { return Task::none() };
            let Some(adapter) = session.adapter.clone() else { return Task::none() };
            let Some(columns) = session.catalog.as_ref().and_then(|c| c.find_table(&table_name)).map(|t| t.columns.clone()) else {
                state.status = Some(format!("Unknown table `{table_name}`"));
                return Task::none();
            };
            let table_for_task = table_name.clone();
            Task::perform(import_file_into_table(adapter, table_name, columns), move |res| {
                Message::ImportFinished(cid, table_for_task.clone(), res)
            })
        }
        Message::ImportFinished(_cid, table_name, result) => {
            state.status = Some(match result {
                Ok(n) => format!("Imported {n} row(s) into {table_name}"),
                Err(e) => format!("Import into {table_name} failed: {e}"),
            });
            Task::none()
        }

        Message::StatusDismissed => {
            state.status = None;
            Task::none()
        }
    }
}

fn with_dialog(state: &mut App, f: impl FnOnce(&mut ConnectionDialog)) -> Task<Message> {
    if let Some(dialog) = &mut state.dialog {
        f(dialog);
    }
    Task::none()
}

async fn export_result_to_file(result: QueryResult, format: ExportFormat, driver: DriverKind) -> Result<PathBuf, String> {
    let (content, default_name, filter_name, extensions): (String, &str, &str, &[&str]) = match format {
        ExportFormat::Csv => (db_core::export::to_csv(&result).map_err(|e| e.to_string())?, "export.csv", "CSV", &["csv"]),
        ExportFormat::Json => (db_core::export::to_json(&result), "export.json", "JSON", &["json"]),
        ExportFormat::SqlInserts => {
            (db_core::export::to_sql_inserts(&result, "query_result", driver), "export.sql", "SQL", &["sql"])
        }
    };

    let handle = rfd::AsyncFileDialog::new()
        .set_file_name(default_name)
        .add_filter(filter_name, extensions)
        .save_file()
        .await
        .ok_or_else(|| "cancelled".to_string())?;
    let path = handle.path().to_path_buf();
    tokio::fs::write(&path, content).await.map_err(|e| e.to_string())?;
    Ok(path)
}

async fn import_file_into_table(
    adapter: Arc<dyn DbAdapter>,
    table_name: String,
    columns: Vec<ColumnInfo>,
) -> Result<u64, String> {
    let handle = rfd::AsyncFileDialog::new()
        .add_filter("CSV or JSON", &["csv", "json"])
        .pick_file()
        .await
        .ok_or_else(|| "cancelled".to_string())?;
    let path = handle.path().to_path_buf();
    let text = tokio::fs::read_to_string(&path).await.map_err(|e| e.to_string())?;

    let is_json = path.extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case("json"));
    let batch = if is_json { db_core::import::parse_json(&text, &columns) } else { db_core::import::parse_csv(&text, &columns) }
        .map_err(|e| e.to_string())?;

    adapter.bulk_insert(&table_name, &batch.columns, &batch.rows).await.map_err(|e| e.to_string())
}
