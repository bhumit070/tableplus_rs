//! MySQL [`DbAdapter`] implementation, built on `sqlx`'s pure-Rust driver
//! (no `libmysqlclient` required on any platform).
//!
//! Like `db-postgres`, one adapter instance owns exactly one physical
//! connection rather than a pool, which keeps [`MySqlAdapter::cancel`]
//! simple: a second, short-lived connection issues `KILL QUERY <id>`
//! against the first connection's `CONNECTION_ID()`.

mod catalog;
mod convert;

use db_core::adapter::DbAdapter;
use db_core::catalog::Catalog;
use db_core::connection::{ConnectionConfig, SslMode};
use db_core::error::{DbError, Result};
use db_core::result::QueryResult;
use db_core::value::Value;
use sqlx::mysql::{MySqlConnectOptions, MySqlConnection, MySqlSslMode};
use sqlx::{Column as _, Row as _};
use std::time::Instant;
use tokio::sync::Mutex;

pub struct MySqlAdapter {
    conn: Mutex<MySqlConnection>,
    connect_options: MySqlConnectOptions,
    connection_id: u64,
    server_version: String,
}

fn build_connect_options(config: &ConnectionConfig, password: &str) -> MySqlConnectOptions {
    let mut opts = MySqlConnectOptions::new()
        .host(&config.host)
        .port(config.port)
        .username(&config.username)
        .password(password)
        .ssl_mode(match config.ssl_mode {
            SslMode::Disable => MySqlSslMode::Disabled,
            SslMode::Prefer => MySqlSslMode::Preferred,
            SslMode::Require => MySqlSslMode::Required,
            SslMode::VerifyFull => MySqlSslMode::VerifyIdentity,
        });

    if let Some(database) = &config.database {
        opts = opts.database(database);
    }
    if config.ssl_mode == SslMode::VerifyFull {
        if let Some(ca_path) = &config.ca_cert_path {
            opts = opts.ssl_ca(ca_path);
        }
    }
    opts
}

/// Opens a new connection. `password` is looked up by the caller (the
/// `app` crate) from the OS keychain, never stored on `config`.
pub async fn connect(config: &ConnectionConfig, password: &str) -> Result<MySqlAdapter> {
    let connect_options = build_connect_options(config, password);
    let mut conn = MySqlConnection::connect_with(&connect_options)
        .await
        .map_err(|e| DbError::Connection(e.to_string()))?;

    let row = sqlx::query("SELECT CONNECTION_ID(), VERSION()")
        .fetch_one(&mut conn)
        .await
        .map_err(|e| DbError::Connection(e.to_string()))?;
    let connection_id: u64 = row.try_get(0).map_err(|e| DbError::Connection(e.to_string()))?;
    let server_version: String = row.try_get(1).map_err(|e| DbError::Connection(e.to_string()))?;

    Ok(MySqlAdapter { conn: Mutex::new(conn), connect_options, connection_id, server_version })
}

#[async_trait::async_trait]
impl DbAdapter for MySqlAdapter {
    fn server_info(&self) -> String {
        format!("MySQL {}", self.server_version)
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
        let mut cancel_conn = MySqlConnection::connect_with(&self.connect_options)
            .await
            .map_err(|e| DbError::Connection(e.to_string()))?;
        sqlx::query(&format!("KILL QUERY {}", self.connection_id))
            .execute(&mut cancel_conn)
            .await
            .map_err(|e| DbError::Query(e.to_string()))?;
        Ok(())
    }

    async fn close(&self) -> Result<()> {
        Ok(())
    }
}

use sqlx::Connection as _;
