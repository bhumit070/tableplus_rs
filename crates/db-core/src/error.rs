use std::fmt;

/// Error type shared by every adapter and by the application layer.
///
/// Adapters translate their driver-specific errors into this type at the
/// boundary so that nothing above `db-core` needs to know which database
/// backend produced a failure.
#[derive(Debug, thiserror::Error)]
pub enum DbError {
    #[error("connection failed: {0}")]
    Connection(String),

    #[error("authentication failed: {0}")]
    Auth(String),

    #[error("query failed: {0}")]
    Query(String),

    #[error("query cancelled")]
    Cancelled,

    #[error("unsupported operation: {0}")]
    Unsupported(String),

    #[error("schema introspection failed: {0}")]
    Introspection(String),

    #[error("data conversion error: {0}")]
    Conversion(String),

    #[error("import/export error: {0}")]
    Io(String),

    #[error("{0}")]
    Other(String),
}

impl DbError {
    pub fn other(msg: impl fmt::Display) -> Self {
        DbError::Other(msg.to_string())
    }
}

pub type Result<T> = std::result::Result<T, DbError>;
