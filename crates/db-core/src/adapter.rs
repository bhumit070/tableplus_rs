use crate::catalog::Catalog;
use crate::error::Result;
use crate::result::QueryResult;
use crate::value::Value;
use async_trait::async_trait;

/// The capability every supported database backend implements.
///
/// `db-core` only defines this trait; concrete implementations live in
/// `db-postgres`, `db-mysql`, and `db-mssql` so that `db-core` never
/// depends on a specific wire protocol or driver crate. The `app` crate
/// holds connections as `Box<dyn DbAdapter>` so the UI can treat every
/// backend uniformly once connected.
#[async_trait]
pub trait DbAdapter: Send + Sync {
    /// Human-readable name of the backend, e.g. `"PostgreSQL 16.2"`.
    fn server_info(&self) -> String;

    /// Run a single SQL statement and return its result set (or affected
    /// row count for DML/DDL). Implementations should support running
    /// arbitrary, multi-statement-free SQL exactly as a user typed it in
    /// the editor.
    async fn execute(&self, sql: &str) -> Result<QueryResult>;

    /// Introspect the server and build a full schema catalog for the
    /// sidebar tree and for autocomplete.
    async fn fetch_catalog(&self) -> Result<Catalog>;

    /// Insert pre-decoded rows into `table` (`columns.len() == each row's
    /// len()`), used by the CSV/JSON import flow. Implementations should
    /// batch this in a transaction where the backend supports it.
    async fn bulk_insert(&self, table: &str, columns: &[String], rows: &[Vec<Value>]) -> Result<u64>;

    /// Best-effort cancellation of whatever statement is currently running
    /// on this connection. Backends that can't support out-of-band
    /// cancellation (yet) should return `DbError::Unsupported`.
    async fn cancel(&self) -> Result<()>;

    /// Close the underlying connection/pool. Called when a connection tab
    /// is closed or the app exits.
    async fn close(&self) -> Result<()>;
}
