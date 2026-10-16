use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Which wire protocol / adapter crate should handle this connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DriverKind {
    Postgres,
    MySql,
    MsSql,
}

impl std::fmt::Display for DriverKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

impl DriverKind {
    pub fn label(&self) -> &'static str {
        match self {
            DriverKind::Postgres => "PostgreSQL",
            DriverKind::MySql => "MySQL",
            DriverKind::MsSql => "SQL Server",
        }
    }

    pub fn default_port(&self) -> u16 {
        match self {
            DriverKind::Postgres => 5432,
            DriverKind::MySql => 3306,
            DriverKind::MsSql => 1433,
        }
    }
}

/// TLS/SSL negotiation mode, deliberately kept small and adapter-agnostic.
/// Each adapter maps these onto its own driver's TLS options.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SslMode {
    /// Never attempt TLS.
    Disable,
    /// Use TLS if the server offers it, but don't fail if it doesn't.
    #[default]
    Prefer,
    /// Require TLS; fail the connection if it can't be negotiated.
    Require,
    /// Require TLS and verify the server certificate against the system
    /// trust store (or `ca_cert_path`, if set).
    VerifyFull,
}

/// Everything needed to open a connection, minus the secret itself.
///
/// The password is deliberately *not* a field here: it is looked up by
/// `id` from the OS keychain (via the `app` crate's `secrets` module) at
/// connection time, so that a `ConnectionConfig` is always safe to
/// serialize, log, and export to disk.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectionConfig {
    pub id: Uuid,
    pub name: String,
    pub driver: DriverKind,
    pub host: String,
    pub port: u16,
    pub username: String,
    /// Default database/catalog to connect to (optional for servers where
    /// it can be chosen after connecting, e.g. MySQL).
    pub database: Option<String>,
    pub ssl_mode: SslMode,
    /// Path to a custom CA certificate, used when `ssl_mode` is
    /// `VerifyFull`. Relative paths are resolved against the config dir.
    pub ca_cert_path: Option<String>,
    /// Free-form key/value connection options forwarded verbatim to the
    /// underlying driver (e.g. Postgres `options`, MySQL `charset`).
    #[serde(default)]
    pub params: Vec<(String, String)>,
    /// Color tag shown in the sidebar, as a TablePlus-style connection
    /// color swatch. Stored as `#rrggbb`.
    #[serde(default)]
    pub color: Option<String>,
}

impl ConnectionConfig {
    pub fn new(name: impl Into<String>, driver: DriverKind) -> Self {
        let port = driver.default_port();
        ConnectionConfig {
            id: Uuid::new_v4(),
            name: name.into(),
            driver,
            host: "localhost".to_string(),
            port,
            username: String::new(),
            database: None,
            ssl_mode: SslMode::default(),
            ca_cert_path: None,
            params: Vec::new(),
            color: None,
        }
    }

    /// The keychain lookup key for this connection's password.
    pub fn secret_key(&self) -> String {
        format!("tableplus-rs:{}", self.id)
    }
}
