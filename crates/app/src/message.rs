use db_core::adapter::DbAdapter;
use db_core::catalog::Catalog;
use db_core::connection::DriverKind;
use db_core::export::ExportFormat;
use db_core::result::QueryResult;
use iced::widget::text_editor;
use std::path::PathBuf;
use std::sync::Arc;
use uuid::Uuid;

#[derive(Clone)]
pub enum Message {
    // --- Connection list / dialog ---
    ShowNewConnectionDialog,
    EditConnection(Uuid),
    CancelDialog,
    DialogNameChanged(String),
    DialogDriverChanged(DriverKind),
    DialogHostChanged(String),
    DialogPortChanged(String),
    DialogUsernameChanged(String),
    DialogPasswordChanged(String),
    DialogDatabaseChanged(String),
    TestConnection,
    TestConnectionFinished(Result<String, String>),
    SaveConnection,
    DeleteConnection(Uuid),
    ExportConnections,
    ImportConnections,
    ConnectionsFileChosen(Option<PathBuf>, DialogIntent),

    // --- Connecting / schema browsing ---
    Connect(Uuid),
    Connected(Uuid, Result<Arc<dyn DbAdapter>, String>),
    CatalogFetched(Uuid, Result<Catalog, String>),
    ToggleSchemaTree(Uuid),
    Disconnect(Uuid),

    // --- Tabs & editor ---
    NewTab(Option<Uuid>),
    CloseTab(usize),
    SwitchTab(usize),
    EditorAction(usize, text_editor::Action),
    AcceptSuggestion(usize, usize),
    DismissSuggestions(usize),
    SelectNextSuggestion(usize),
    SelectPrevSuggestion(usize),

    // --- Query execution ---
    RunQuery(usize),
    QueryFinished(usize, Result<QueryResult, String>),
    CancelQuery(usize),

    // --- Export / Import data ---
    ExportResultRequested(usize, ExportFormat),
    ExportFinished(usize, Result<PathBuf, String>),
    ImportIntoTableRequested(Uuid, String),
    ImportFinished(Uuid, String, Result<u64, String>),

    StatusDismissed,
}

#[derive(Clone, Copy)]
pub enum DialogIntent {
    ExportConnections,
    ImportConnections,
}
