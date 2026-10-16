//! Dispatches a [`ConnectionConfig`] to the right adapter crate. This is
//! the one place in the `app` crate that knows all three adapter crates
//! exist; everywhere else talks to `Box<dyn DbAdapter>` / `Arc<dyn
//! DbAdapter>` from `db-core`.

use db_core::adapter::DbAdapter;
use db_core::connection::{ConnectionConfig, DriverKind};
use db_core::error::Result;

pub async fn connect(config: ConnectionConfig, password: String) -> Result<Box<dyn DbAdapter>> {
    match config.driver {
        DriverKind::Postgres => {
            let adapter = db_postgres::connect(&config, &password).await?;
            Ok(Box::new(adapter))
        }
        DriverKind::MySql => {
            let adapter = db_mysql::connect(&config, &password).await?;
            Ok(Box::new(adapter))
        }
        DriverKind::MsSql => {
            let adapter = db_mssql::connect(&config, &password).await?;
            Ok(Box::new(adapter))
        }
    }
}
