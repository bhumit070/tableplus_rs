use db_core::error::{DbError, Result};
use db_core::value::Value;
use sqlx::postgres::{PgConnection, PgRow};
use sqlx::{Row as _, ValueRef as _};

/// Decodes column `idx` of `row` into a backend-agnostic [`Value`], using
/// the Postgres type name (`INT4`, `TEXT`, `NUMERIC`, ...) reported by
/// sqlx to pick the right Rust type to decode through.
///
/// A real-world schema inevitably has columns this doesn't have a
/// dedicated arm for — custom enums, arrays of types we don't special-case,
/// `inet`/`macaddr`/`money`/PostGIS geometry, domains over non-text base
/// types, and so on. sqlx's `Decode` rejects those as a Rust `String`
/// outright (it checks OID compatibility before even looking at the
/// bytes), so the old fallback — "try to decode as text, and propagate
/// the error if that fails" — meant a single exotic column aborted the
/// *entire* query. That's unacceptable for a general-purpose SQL client:
/// one unrecognized column shouldn't make `SELECT *` unusable. This never
/// returns an error for a type-decode failure; it degrades to a readable
/// placeholder and logs a warning instead.
pub(crate) fn decode_value(row: &PgRow, idx: usize, type_name: &str) -> Result<Value> {
    let raw = row
        .try_get_raw(idx)
        .map_err(|e| DbError::Conversion(e.to_string()))?;
    if raw.is_null() {
        return Ok(Value::Null);
    }

    let decoded = match type_name {
        "BOOL" => row.try_get::<bool, _>(idx).map(Value::Bool),
        "INT2" => row.try_get::<i16, _>(idx).map(|v| Value::I64(v as i64)),
        "INT4" => row.try_get::<i32, _>(idx).map(|v| Value::I64(v as i64)),
        "INT8" => row.try_get::<i64, _>(idx).map(Value::I64),
        "FLOAT4" => row.try_get::<f32, _>(idx).map(|v| Value::F64(v as f64)),
        "FLOAT8" => row.try_get::<f64, _>(idx).map(Value::F64),
        "NUMERIC" => row.try_get::<rust_decimal::Decimal, _>(idx).map(Value::Decimal),
        "TEXT" | "VARCHAR" | "BPCHAR" | "NAME" | "CHAR" | "CITEXT" => {
            row.try_get::<String, _>(idx).map(Value::Text)
        }
        "UUID" => row.try_get::<uuid::Uuid, _>(idx).map(Value::Uuid),
        "JSON" | "JSONB" => row.try_get::<serde_json::Value, _>(idx).map(Value::Json),
        "DATE" => row.try_get::<chrono::NaiveDate, _>(idx).map(Value::Date),
        "TIME" => row.try_get::<chrono::NaiveTime, _>(idx).map(Value::Time),
        "TIMESTAMP" => row.try_get::<chrono::NaiveDateTime, _>(idx).map(Value::DateTime),
        "TIMESTAMPTZ" => row
            .try_get::<chrono::DateTime<chrono::Utc>, _>(idx)
            .map(Value::TimestampTz),
        "BYTEA" => row.try_get::<Vec<u8>, _>(idx).map(Value::Bytes),
        "_TEXT" | "_VARCHAR" => row
            .try_get::<Vec<String>, _>(idx)
            .map(|items| Value::Array(items.into_iter().map(Value::Text).collect())),
        "_INT4" => row
            .try_get::<Vec<i32>, _>(idx)
            .map(|items| Value::Array(items.into_iter().map(|v| Value::I64(v as i64)).collect())),
        "_INT8" => row
            .try_get::<Vec<i64>, _>(idx)
            .map(|items| Value::Array(items.into_iter().map(Value::I64).collect())),
        _ => {
            // Most unrecognized scalar types (enums, domains over text,
            // `inet`/`macaddr`, etc.) still decode fine as Rust `String`
            // since Postgres can render them as text; this just isn't
            // true for *every* OID (arrays we don't special-case, binary
            // domains, composite/range types) — the `unwrap_or_else`
            // below is what keeps those from aborting the whole query.
            row.try_get::<String, _>(idx).map(Value::Text)
        }
    };

    Ok(decoded.unwrap_or_else(|e| {
        tracing::warn!(column = idx, type_name, error = %e, "could not decode column value as its reported type; showing a placeholder");
        Value::Text(format!("<{type_name}>"))
    }))
}

fn quote_qualified_ident(ident: &str) -> String {
    ident
        .split('.')
        .map(|part| format!("\"{}\"", part.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(".")
}

/// Binds `rows` into `INSERT INTO table (columns) VALUES (...)` statements
/// and executes them inside a single transaction, so a partial failure
/// leaves no rows behind.
pub(crate) async fn bulk_insert(
    conn: &mut PgConnection,
    table: &str,
    columns: &[String],
    rows: &[Vec<Value>],
) -> Result<u64> {
    if rows.is_empty() {
        return Ok(0);
    }

    let quoted_table = quote_qualified_ident(table);
    let quoted_columns: Vec<String> = columns.iter().map(|c| quote_qualified_ident(c)).collect();
    let placeholders: Vec<String> = (1..=columns.len()).map(|i| format!("${i}")).collect();
    let sql = format!(
        "INSERT INTO {quoted_table} ({}) VALUES ({})",
        quoted_columns.join(", "),
        placeholders.join(", ")
    );

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
    query: sqlx::query::Query<'q, sqlx::Postgres, sqlx::postgres::PgArguments>,
    value: &'q Value,
) -> Result<sqlx::query::Query<'q, sqlx::Postgres, sqlx::postgres::PgArguments>> {
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
        Value::TimestampTz(dt) => query.bind(*dt),
        Value::Uuid(u) => query.bind(*u),
        Value::Json(j) => query.bind(j),
        Value::Array(_) => {
            return Err(DbError::Unsupported(
                "importing array-typed columns is not supported yet".to_string(),
            ))
        }
    })
}

use sqlx::Connection as _;
