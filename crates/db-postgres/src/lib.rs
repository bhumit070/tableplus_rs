//! PostgreSQL [`DbAdapter`] implementation, built on `sqlx`'s pure-Rust,
//! rustls-based driver (no `libpq` or other native client library
//! required on any platform).
//!
//! One adapter instance owns exactly one physical connection (not a
//! pool): a desktop DB client runs one statement at a time per open
//! connection tab, and a single connection is what makes
//! [`PostgresAdapter::cancel`] correct and simple (a *second*, short-lived
//! connection calls `pg_cancel_backend` on the first one's backend PID).

mod catalog;
mod convert;

use db_core::adapter::DbAdapter;
use db_core::catalog::Catalog;
use db_core::connection::{ConnectionConfig, SslMode};
use db_core::error::{DbError, Result};
use db_core::result::QueryResult;
use db_core::value::Value;
use sqlx::postgres::{PgConnectOptions, PgConnection, PgSslMode};
use sqlx::{Column as _, Row as _};
use std::time::Instant;
use tokio::sync::Mutex;

pub struct PostgresAdapter {
    conn: Mutex<PgConnection>,
    connect_options: PgConnectOptions,
    backend_pid: i32,
    server_version: String,
}

fn build_connect_options(config: &ConnectionConfig, password: &str) -> PgConnectOptions {
    let mut opts = PgConnectOptions::new()
        .host(&config.host)
        .port(config.port)
        .username(&config.username)
        .password(password)
        .ssl_mode(match config.ssl_mode {
            SslMode::Disable => PgSslMode::Disable,
            SslMode::Prefer => PgSslMode::Prefer,
            SslMode::Require => PgSslMode::Require,
            SslMode::VerifyFull => PgSslMode::VerifyFull,
        });

    if let Some(database) = &config.database {
        opts = opts.database(database);
    }
    if config.ssl_mode == SslMode::VerifyFull {
        if let Some(ca_path) = &config.ca_cert_path {
            opts = opts.ssl_root_cert(ca_path);
        }
    }
    if !config.params.is_empty() {
        opts = opts.options(config.params.iter().map(|(k, v)| (k.as_str(), v.as_str())));
    }
    opts
}

/// Opens a new connection. `password` is looked up by the caller (the
/// `app` crate) from the OS keychain, never stored on `config`.
pub async fn connect(config: &ConnectionConfig, password: &str) -> Result<PostgresAdapter> {
    let connect_options = build_connect_options(config, password);
    let mut conn = PgConnection::connect_with(&connect_options)
        .await
        .map_err(|e| DbError::Connection(e.to_string()))?;

    let row = sqlx::query("SELECT pg_backend_pid(), version()")
        .fetch_one(&mut conn)
        .await
        .map_err(|e| DbError::Connection(e.to_string()))?;
    let backend_pid: i32 = row.try_get(0).map_err(|e| DbError::Connection(e.to_string()))?;
    let server_version: String = row.try_get(1).map_err(|e| DbError::Connection(e.to_string()))?;

    Ok(PostgresAdapter { conn: Mutex::new(conn), connect_options, backend_pid, server_version })
}

#[async_trait::async_trait]
impl DbAdapter for PostgresAdapter {
    fn server_info(&self) -> String {
        self.server_version.clone()
    }

    async fn execute(&self, sql: &str) -> Result<QueryResult> {
        let mut conn = self.conn.lock().await;
        let started = Instant::now();
        let rows = sqlx::query(sql)
            .fetch_all(&mut *conn)
            .await
            .map_err(|e| DbError::Query(e.to_string()))?;
        let duration = started.elapsed();

        let columns = rows
            .first()
            .map(|r| {
                r.columns()
                    .iter()
                    .map(|c| db_core::result::Column {
                        name: c.name().to_string(),
                        type_name: c.type_info().to_string(),
                        nullable: true,
                    })
                    .collect()
            })
            .unwrap_or_default();

        let mut decoded_rows = Vec::with_capacity(rows.len());
        for row in &rows {
            let mut decoded = Vec::with_capacity(row.columns().len());
            for (idx, col) in row.columns().iter().enumerate() {
                decoded.push(convert::decode_value(row, idx, &col.type_info().to_string())?);
            }
            decoded_rows.push(decoded);
        }

        Ok(QueryResult { columns, rows: decoded_rows, rows_affected: None, duration })
    }

    async fn fetch_catalog(&self) -> Result<Catalog> {
        let mut conn = self.conn.lock().await;
        catalog::fetch_catalog(&mut conn).await
    }

    async fn bulk_insert(&self, table: &str, columns: &[String], rows: &[Vec<Value>]) -> Result<u64> {
        let mut conn = self.conn.lock().await;
        convert::bulk_insert(&mut conn, table, columns, rows).await
    }

    async fn cancel(&self) -> Result<()> {
        // A dedicated short-lived connection, independent of the mutex
        // held by whatever statement is currently running on `self.conn`.
        let mut cancel_conn = sqlx::postgres::PgConnection::connect_with(&self.connect_options)
            .await
            .map_err(|e| DbError::Connection(e.to_string()))?;
        sqlx::query("SELECT pg_cancel_backend($1)")
            .bind(self.backend_pid)
            .execute(&mut cancel_conn)
            .await
            .map_err(|e| DbError::Query(e.to_string()))?;
        Ok(())
    }

    async fn close(&self) -> Result<()> {
        // Dropping the connection on the next lock acquisition closes the
        // socket; sqlx has no explicit async close on a bare `PgConnection`
        // short of dropping it, which happens when the adapter itself is
        // dropped by the `app` crate.
        Ok(())
    }
}

use sqlx::Connection as _;
