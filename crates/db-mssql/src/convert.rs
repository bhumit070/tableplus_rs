use db_core::error::{DbError, Result};
use db_core::value::Value;
use tiberius::{ColumnData, ColumnType, Row};

/// Decodes column `idx` of `row` into a backend-agnostic [`Value`].
///
/// Matched against the `Debug` representation of tiberius's [`ColumnType`]
/// rather than its variants directly, since new TDS type codes get added
/// upstream over time; falling back to the server's text rendering for
/// anything unrecognized keeps this forward-compatible.
pub(crate) fn decode_value(row: &Row, idx: usize, column_type: ColumnType) -> Result<Value> {
    let ty = format!("{column_type:?}").to_uppercase();

    let value = if ty.contains("BIT") {
        row.get::<bool, _>(idx).map(Value::Bool)
    } else if ty.contains("INT8") {
        row.get::<i64, _>(idx).map(Value::I64)
    } else if ty.contains("INT1") || ty.contains("INT2") || ty.contains("INT4") {
        row.get::<i32, _>(idx).map(|v| Value::I64(v as i64))
    } else if ty.contains("FLOAT4") {
        row.get::<f32, _>(idx).map(|v| Value::F64(v as f64))
    } else if ty.contains("FLOAT8") || ty.contains("MONEY") {
        row.get::<f64, _>(idx).map(Value::F64)
    } else if ty.contains("NUMERIC") || ty.contains("DECIMAL") {
        row.get::<rust_decimal::Decimal, _>(idx).map(Value::Decimal)
    } else if ty.contains("GUID") {
        row.get::<uuid::Uuid, _>(idx).map(Value::Uuid)
    } else if ty.contains("DATETIMEOFFSET") {
        row.get::<chrono::DateTime<chrono::Utc>, _>(idx).map(Value::TimestampTz)
    } else if ty.contains("DATETIME") {
        row.get::<chrono::NaiveDateTime, _>(idx).map(Value::DateTime)
    } else if ty.contains("DATEN") || ty == "DATE" {
        row.get::<chrono::NaiveDate, _>(idx).map(Value::Date)
    } else if ty.contains("TIME") {
        row.get::<chrono::NaiveTime, _>(idx).map(Value::Time)
    } else if ty.contains("BINARY") || ty.contains("IMAGE") {
        row.get::<&[u8], _>(idx).map(|b| Value::Bytes(b.to_vec()))
    } else {
        row.get::<&str, _>(idx).map(|s| Value::Text(s.to_string()))
    };

    Ok(value.unwrap_or(Value::Null))
}

fn quote_qualified_ident(ident: &str) -> String {
    ident
        .split('.')
        .map(|part| format!("[{}]", part.replace(']', "]]")))
        .collect::<Vec<_>>()
        .join(".")
}

/// Local wrapper so we can implement the foreign `tiberius::ToSql` trait
/// for `db_core::Value` (also foreign to this crate) without violating
/// Rust's orphan rules.
struct BoundValue<'a>(&'a Value);

impl tiberius::ToSql for BoundValue<'_> {
    fn to_sql(&self) -> ColumnData<'_> {
        match self.0 {
            Value::Null => ColumnData::String(None),
            Value::Bool(b) => ColumnData::Bit(Some(*b)),
            Value::I64(i) => ColumnData::I64(Some(*i)),
            Value::F64(f) => ColumnData::F64(Some(*f)),
            Value::Decimal(d) => d.to_sql(),
            Value::Text(s) => ColumnData::String(Some(std::borrow::Cow::Borrowed(s))),
            Value::Bytes(b) => ColumnData::Binary(Some(std::borrow::Cow::Borrowed(b))),
            Value::Date(d) => d.to_sql(),
            Value::Time(t) => t.to_sql(),
            Value::DateTime(dt) => dt.to_sql(),
            Value::TimestampTz(dt) => dt.to_sql(),
            Value::Uuid(u) => u.to_sql(),
            Value::Json(j) => ColumnData::String(Some(std::borrow::Cow::Owned(j.to_string()))),
            Value::Array(_) => ColumnData::String(None),
        }
    }
}

/// Binds `rows` into `INSERT INTO table (columns) VALUES (@P1, ...)`
/// statements and executes them inside a single transaction.
pub(crate) async fn bulk_insert(
    client: &mut crate::TdsClient,
    table: &str,
    columns: &[String],
    rows: &[Vec<Value>],
) -> Result<u64> {
    if rows.is_empty() {
        return Ok(0);
    }

    let quoted_table = quote_qualified_ident(table);
    let quoted_columns: Vec<String> = columns.iter().map(|c| quote_qualified_ident(c)).collect();
    let placeholders: Vec<String> = (1..=columns.len()).map(|i| format!("@P{i}")).collect();
    let sql = format!(
        "INSERT INTO {quoted_table} ({}) VALUES ({})",
        quoted_columns.join(", "),
        placeholders.join(", ")
    );

    client.simple_query("BEGIN TRANSACTION").await.map_err(|e| DbError::Query(e.to_string()))?;

    let mut inserted = 0u64;
    for row in rows {
        let bound: Vec<BoundValue<'_>> = row.iter().map(BoundValue).collect();
        let params: Vec<&dyn tiberius::ToSql> = bound.iter().map(|b| b as &dyn tiberius::ToSql).collect();
        let result = match client.execute(&sql, &params).await {
            Ok(r) => r,
            Err(e) => {
                let _ = client.simple_query("ROLLBACK TRANSACTION").await;
                return Err(DbError::Query(e.to_string()));
            }
        };
        inserted += result.total();
    }

    client.simple_query("COMMIT TRANSACTION").await.map_err(|e| DbError::Query(e.to_string()))?;
    Ok(inserted)
}
