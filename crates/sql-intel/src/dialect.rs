use db_core::connection::DriverKind;
use sqlparser::dialect::{Dialect, GenericDialect, MsSqlDialect, MySqlDialect, PostgreSqlDialect};

/// Picks the `sqlparser` dialect matching a connection's backend, so
/// tokenization (and therefore both syntax highlighting and autocomplete)
/// understands each backend's quoting and operator differences (e.g.
/// MySQL's backtick identifiers vs Postgres's double quotes, or T-SQL's
/// `[brackets]`).
pub fn dialect_for(driver: DriverKind) -> Box<dyn Dialect> {
    match driver {
        DriverKind::Postgres => Box::new(PostgreSqlDialect {}),
        DriverKind::MySql => Box::new(MySqlDialect {}),
        DriverKind::MsSql => Box::new(MsSqlDialect {}),
    }
}

/// Dialect-agnostic fallback, used when no connection is active yet (e.g.
/// a scratch SQL tab not yet bound to a connection).
pub fn generic_dialect() -> Box<dyn Dialect> {
    Box::new(GenericDialect {})
}
