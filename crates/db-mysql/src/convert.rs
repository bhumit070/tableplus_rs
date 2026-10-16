use db_core::error::{DbError, Result};
use db_core::value::Value;
use sqlx::mysql::{MySqlConnection, MySqlRow};
use sqlx::{Row as _, ValueRef as _};

/// Decodes column `idx` of `row` into a backend-agnostic [`Value`].
///
/// MySQL has no native boolean type (`BOOLEAN` is a `TINYINT(1)` alias),
/// so `TINYINT` columns are decoded as [`Value::I64`] rather than guessed
/// at as booleans — that ambiguity is left to the caller/UI, same as
/// every other MySQL client.
pub(crate) fn decode_value(row: &MySqlRow, idx: usize, type_name: &str) -> Result<Value> {
    let raw = row.try_get_raw(idx).map_err(|e| DbError::Conversion(e.to_string()))?;
    if raw.is_null() {
        return Ok(Value::Null);
    }

    let ty = type_name.to_ascii_uppercase();
    let decoded = if ty.contains("DATETIME") || ty.contains("TIMESTAMP") {
        row.try_get::<chrono::NaiveDateTime, _>(idx).map(Value::DateTime)
    } else if ty == "DATE" {
        row.try_get::<chrono::NaiveDate, _>(idx).map(Value::Date)
    } else if ty.contains("TIME") {
        row.try_get::<chrono::NaiveTime, _>(idx).map(Value::Time)
    } else if ty.contains("DECIMAL") || ty.contains("NUMERIC") {
        row.try_get::<rust_decimal::Decimal, _>(idx).map(Value::Decimal)
    } else if ty.contains("UNSIGNED") && (ty.contains("INT") || ty.contains("SERIAL")) {
        row.try_get::<u64, _>(idx).map(|v| Value::I64(v as i64))
    } else if ty.contains("INT") || ty.contains("YEAR") {
        row.try_get::<i64, _>(idx).map(Value::I64)
    } else if ty.contains("FLOAT") {
        row.try_get::<f32, _>(idx).map(|v| Value::F64(v as f64))
    } else if ty.contains("DOUBLE") {
        row.try_get::<f64, _>(idx).map(Value::F64)
    } else if ty.contains("JSON") {
        row.try_get::<serde_json::Value, _>(idx).map(Value::Json)
    } else if ty.contains("BLOB") || ty.contains("BINARY") {
        row.try_get::<Vec<u8>, _>(idx).map(Value::Bytes)
    } else {
        // VARCHAR, CHAR, TEXT, ENUM, SET, and anything else we don't
        // special-case decode cleanly as text.
        row.try_get::<String, _>(idx).map(Value::Text)
    };

    // A single unrecognized/exotic column (a spatial type, a custom
    // collation sqlx can't map, etc.) shouldn't abort the whole query —
    // degrade to a readable placeholder instead of failing the row.
    Ok(decoded.unwrap_or_else(|e| {
        tracing::warn!(column = idx, type_name, error = %e, "could not decode column value as its reported type; showing a placeholder");
        Value::Text(format!("<{type_name}>"))
    }))
}

fn quote_qualified_ident(ident: &str) -> String {
    ident
        .split('.')
        .map(|part| format!("`{}`", part.replace('`', "``")))
        .collect::<Vec<_>>()
        .join(".")
}

/// Binds `rows` into `INSERT INTO table (columns) VALUES (...)` statements
/// and executes them inside a single transaction.
pub(crate) async fn bulk_insert(
    conn: &mut MySqlConnection,
    table: &str,
    columns: &[String],
    rows: &[Vec<Value>],
) -> Result<u64> {
    if rows.is_empty() {
        return Ok(0);
    }

    let quoted_table = quote_qualified_ident(table);
    let quoted_columns: Vec<String> = columns.iter().map(|c| quote_qualified_ident(c)).collect();
    let placeholders = vec!["?"; columns.len()].join(", ");
    let sql = format!("INSERT INTO {quoted_table} ({}) VALUES ({placeholders})", quoted_columns.join(", "));

    let mut tx = conn.begin().await.map_err(|e| DbError::Query(e.to_string()))?;
    let mut inserted = 0u64;
    for row in rows {
        let mut query = sqlx::query(&sql);
        for value in row {
            query = bind_value(query, value)?;
        }
        let result = query.execute(&mut *tx).await.map_err(|e| DbError::Query(e.to_string()))?;
        inserted += result.rows_affected();
    }
    tx.commit().await.map_err(|e| DbError::Query(e.to_string()))?;
    Ok(inserted)
}

fn bind_value<'q>(
    query: sqlx::query::Query<'q, sqlx::MySql, sqlx::mysql::MySqlArguments>,
    value: &'q Value,
) -> Result<sqlx::query::Query<'q, sqlx::MySql, sqlx::mysql::MySqlArguments>> {
    Ok(match value {
        Value::Null => query.bind(Option::<String>::None),
        Value::Bool(b) => query.bind(*b),
        Value::I64(i) => query.bind(*i),
        Value::F64(f) => query.bind(*f),
        Value::Decimal(d) => query.bind(*d),
        Value::Text(s) => query.bind(s),
        Value::Bytes(b) => query.bind(b),
        Value::Date(d) => query.bind(*d),
        Value::Time(t) => query.bind(*t),
        Value::DateTime(dt) => query.bind(*dt),
        Value::TimestampTz(dt) => query.bind(dt.naive_utc()),
        Value::Uuid(u) => query.bind(u.to_string()),
        Value::Json(j) => query.bind(j),
        Value::Array(_) => {
            return Err(DbError::Unsupported("MySQL has no array column type to import into".to_string()))
        }
    })
}

use sqlx::Connection as _;
