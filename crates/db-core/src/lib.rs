//! Backend-agnostic core: the types and traits that `db-postgres`,
//! `db-mysql`, `db-mssql`, `sql-intel`, and the `app` GUI all share.
//!
//! Nothing in this crate knows how to actually talk to a database — see
//! the adapter crates for that. This crate only defines the shape of a
//! connection, a query result, a schema catalog, and the import/export
//! transforms that operate on them.

pub mod adapter;
pub mod catalog;
pub mod connection;
pub mod connection_set;
pub mod error;
pub mod export;
pub mod import;
pub mod result;
pub mod value;

pub use adapter::DbAdapter;
pub use catalog::Catalog;
pub use connection::{ConnectionConfig, DriverKind, SslMode};
pub use error::{DbError, Result};
pub use result::{Column, QueryResult};
pub use value::Value;
