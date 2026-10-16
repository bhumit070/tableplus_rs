//! SQL Server [`DbAdapter`] implementation, built on `tiberius`, a
//! pure-Rust TDS client (no ODBC driver or `msodbcsql` install required on
//! any platform).
//!
//! SQL Server authentication only (no Windows Integrated Auth, which
//! would require platform-specific native libraries and defeat the
//! "install nothing, run anywhere" goal). One adapter instance owns one
//! physical connection; [`MsSqlAdapter::cancel`] opens a second,
//! short-lived connection and runs `KILL <spid>`, which requires the
//! connecting login to have `ALTER ANY CONNECTION` permission (true of
//! `sa`, and of most admin-provisioned accounts).

mod catalog;
mod convert;

use db_core::adapter::DbAdapter;
use db_core::catalog::Catalog;
use db_core::connection::{ConnectionConfig, SslMode};
use db_core::error::{DbError, Result};
use db_core::result::QueryResult;
use db_core::value::Value;
use tiberius::{AuthMethod, Client, Config, EncryptionLevel};
use tokio::net::TcpStream;
use tokio::sync::Mutex;
use tokio_util::compat::{Compat, TokioAsyncWriteCompatExt};

pub(crate) type TdsClient = Client<Compat<TcpStream>>;

pub struct MsSqlAdapter {
    client: Mutex<TdsClient>,
    config: Config,
    spid: i16,
    server_version: String,
}

fn build_config(config: &ConnectionConfig, password: &str) -> Config {
    let mut tds_config = Config::new();
    tds_config.host(&config.host);
    tds_config.port(config.port);
    tds_config.authentication(AuthMethod::sql_server(&config.username, password));
    if let Some(database) = &config.database {
        tds_config.database(database);
    }
    tds_config.encryption(match config.ssl_mode {
        SslMode::Disable => EncryptionLevel::NotSupported,
        SslMode::Prefer => EncryptionLevel::On,
        SslMode::Require | SslMode::VerifyFull => EncryptionLevel::Required,
    });
    // TODO(#1): wire `ca_cert_path` through for `SslMode::VerifyFull` once
    // tiberius exposes a custom-root-CA hook; until then we trust
    // self-signed certs so `Require`/`VerifyFull` still negotiate TLS
    // against servers with the default auto-generated certificate.
    tds_config.trust_cert();
    tds_config
}

async fn connect_client(config: &Config) -> Result<TdsClient> {
    let tcp = TcpStream::connect(config.get_addr())
        .await
        .map_err(|e| DbError::Connection(e.to_string()))?;
    tcp.set_nodelay(true).map_err(|e| DbError::Connection(e.to_string()))?;
    Client::connect(config.clone(), tcp.compat_write())
        .await
        .map_err(|e| DbError::Connection(e.to_string()))
}

/// Opens a new connection. `password` is looked up by the caller (the
/// `app` crate) from the OS keychain, never stored on `config`.
pub async fn connect(connection_config: &ConnectionConfig, password: &str) -> Result<MsSqlAdapter> {
    let config = build_config(connection_config, password);
    let mut client = connect_client(&config).await?;

    let row = client
        .simple_query("SELECT @@SPID, @@VERSION")
        .await
        .map_err(|e| DbError::Connection(e.to_string()))?
        .into_row()
        .await
        .map_err(|e| DbError::Connection(e.to_string()))?
        .ok_or_else(|| DbError::Connection("server returned no rows for @@SPID".to_string()))?;

    let spid: i16 = row.get(0).ok_or_else(|| DbError::Connection("missing @@SPID".to_string()))?;
    let server_version: String =
        row.get::<&str, _>(1).unwrap_or("SQL Server").to_string();

    Ok(MsSqlAdapter { client: Mutex::new(client), config, spid, server_version })
}

#[async_trait::async_trait]
impl DbAdapter for MsSqlAdapter {
    fn server_info(&self) -> String {
        self.server_version.clone()
    }

    async fn execute(&self, sql: &str) -> Result<QueryResult> {
        let mut client = self.client.lock().await;
        let started = std::time::Instant::now();

        let stream = client.simple_query(sql).await.map_err(|e| DbError::Query(e.to_string()))?;
        let result_sets = stream.into_results().await.map_err(|e| DbError::Query(e.to_string()))?;
        let duration = started.elapsed();

        let Some(rows) = result_sets.into_iter().find(|rs| !rs.is_empty()) else {
            return Ok(QueryResult { columns: Vec::new(), rows: Vec::new(), rows_affected: Some(0), duration });
        };

        let columns = rows[0]
            .columns()
            .iter()
            .map(|c| db_core::result::Column {
                name: c.name().to_string(),
                type_name: format!("{:?}", c.column_type()),
                nullable: true,
            })
            .collect();

        let mut decoded_rows = Vec::with_capacity(rows.len());
        for row in &rows {
            let mut decoded = Vec::with_capacity(row.columns().len());
            for (idx, col) in row.columns().iter().enumerate() {
                decoded.push(convert::decode_value(row, idx, col.column_type())?);
            }
            decoded_rows.push(decoded);
        }

        Ok(QueryResult { columns, rows: decoded_rows, rows_affected: None, duration })
    }

    async fn fetch_catalog(&self) -> Result<Catalog> {
        let mut client = self.client.lock().await;
        catalog::fetch_catalog(&mut client).await
    }

    async fn bulk_insert(&self, table: &str, columns: &[String], rows: &[Vec<Value>]) -> Result<u64> {
        let mut client = self.client.lock().await;
        convert::bulk_insert(&mut client, table, columns, rows).await
    }

    async fn cancel(&self) -> Result<()> {
        let mut cancel_client = connect_client(&self.config).await?;
        cancel_client
            .simple_query(format!("KILL {}", self.spid))
            .await
            .map_err(|e| DbError::Query(e.to_string()))?;
        Ok(())
    }

    async fn close(&self) -> Result<()> {
        Ok(())
    }
}
