use db_core::adapter::DbAdapter;
use db_core::catalog::Catalog;
use db_core::connection::{ConnectionConfig, DriverKind};
use db_core::result::QueryResult;
use iced::widget::text_editor;
use std::collections::HashMap;
use std::sync::Arc;
use uuid::Uuid;

pub struct App {
    pub connections: Vec<ConnectionConfig>,
    pub sessions: HashMap<Uuid, Session>,
    pub tabs: Vec<Tab>,
    pub active_tab: usize,
    pub dialog: Option<ConnectionDialog>,
    pub status: Option<String>,
    pub expanded_connections: Vec<Uuid>,
}

pub struct Session {
    pub config: ConnectionConfig,
    pub adapter: Option<Arc<dyn DbAdapter>>,
    pub catalog: Option<Catalog>,
    pub connecting: bool,
    pub error: Option<String>,
}

pub struct Tab {
    pub id: Uuid,
    pub connection_id: Option<Uuid>,
    pub editor: text_editor::Content,
    pub result: Option<QueryResult>,
    pub error: Option<String>,
    pub running: bool,
    pub suggestions: Vec<sql_intel::Suggestion>,
    pub selected_suggestion: usize,
}

impl Tab {
    pub fn new(connection_id: Option<Uuid>) -> Self {
        Tab {
            id: Uuid::new_v4(),
            connection_id,
            editor: text_editor::Content::new(),
            result: None,
            error: None,
            running: false,
            suggestions: Vec::new(),
            selected_suggestion: 0,
        }
    }

    pub fn title(&self, app: &App) -> String {
        match self.connection_id.and_then(|id| app.sessions.get(&id)) {
            Some(session) => session.config.name.clone(),
            None => "New Query".to_string(),
        }
    }
}

pub struct ConnectionDialog {
    pub editing_id: Option<Uuid>,
    pub name: String,
    pub driver: DriverKind,
    pub host: String,
    pub port: String,
    pub username: String,
    pub password: String,
    pub database: String,
    pub test_result: Option<Result<String, String>>,
    pub testing: bool,
}

impl ConnectionDialog {
    pub fn new() -> Self {
        let driver = DriverKind::Postgres;
        ConnectionDialog {
            editing_id: None,
            name: String::new(),
            driver,
            host: "localhost".to_string(),
            port: driver.default_port().to_string(),
            username: String::new(),
            password: String::new(),
            database: String::new(),
            test_result: None,
            testing: false,
        }
    }

    pub fn from_config(config: &ConnectionConfig, password: Option<String>) -> Self {
        ConnectionDialog {
            editing_id: Some(config.id),
            name: config.name.clone(),
            driver: config.driver,
            host: config.host.clone(),
            port: config.port.to_string(),
            username: config.username.clone(),
            password: password.unwrap_or_default(),
            database: config.database.clone().unwrap_or_default(),
            test_result: None,
            testing: false,
        }
    }

    pub fn to_config(&self) -> ConnectionConfig {
        let mut config = ConnectionConfig::new(self.name.clone(), self.driver);
        if let Some(id) = self.editing_id {
            config.id = id;
        }
        config.host = self.host.clone();
        config.port = self.port.parse().unwrap_or(self.driver.default_port());
        config.username = self.username.clone();
        config.database = if self.database.is_empty() { None } else { Some(self.database.clone()) };
        config
    }
}

impl App {
    pub fn boot() -> App {
        let connections = crate::store::connections::load().unwrap_or_default();
        App {
            connections,
            sessions: HashMap::new(),
            tabs: vec![Tab::new(None)],
            active_tab: 0,
            dialog: None,
            status: None,
            expanded_connections: Vec::new(),
        }
    }

    pub fn title(&self) -> String {
        "TablePlus-rs".to_string()
    }

    pub fn theme(&self) -> iced::Theme {
        iced::Theme::Dark
    }

    pub fn find_connection(&self, id: Uuid) -> Option<&ConnectionConfig> {
        self.connections.iter().find(|c| c.id == id)
    }
}
