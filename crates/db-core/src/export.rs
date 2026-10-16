//! Exporting a [`QueryResult`] (a ran query, or a browsed table) to a file
//! format a user can take elsewhere. These are pure, adapter-agnostic data
//! transforms: the `app` crate wires them to "Export..." menu items and a
//! file-save dialog.

use crate::connection::DriverKind;
use crate::result::QueryResult;
use crate::value::Value;
use std::fmt::Write as _;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportFormat {
    Csv,
    Json,
    /// `INSERT INTO ...` statements, quoted for the given dialect.
    SqlInserts,
}

/// Renders `result` as RFC 4180 CSV, including a header row.
pub fn to_csv(result: &QueryResult) -> crate::error::Result<String> {
    let mut writer = csv::WriterBuilder::new().from_writer(Vec::new());
    let headers: Vec<&str> = result.columns.iter().map(|c| c.name.as_str()).collect();
    writer
        .write_record(&headers)
        .map_err(|e| crate::error::DbError::Io(e.to_string()))?;
    for row in &result.rows {
        let record: Vec<String> = row.iter().map(render_csv_cell).collect();
        writer
            .write_record(&record)
            .map_err(|e| crate::error::DbError::Io(e.to_string()))?;
    }
    let bytes = writer
        .into_inner()
        .map_err(|e| crate::error::DbError::Io(e.to_string()))?;
    String::from_utf8(bytes).map_err(|e| crate::error::DbError::Io(e.to_string()))
}

fn render_csv_cell(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        other => other.display(),
    }
}

/// Renders `result` as a JSON array of objects, one per row, keyed by
/// column name.
pub fn to_json(result: &QueryResult) -> String {
    let rows: Vec<serde_json::Value> = result
        .rows
        .iter()
        .map(|row| {
            let mut obj = serde_json::Map::with_capacity(result.columns.len());
            for (col, value) in result.columns.iter().zip(row.iter()) {
                obj.insert(col.name.clone(), value.to_json());
            }
            serde_json::Value::Object(obj)
        })
        .collect();
    serde_json::to_string_pretty(&serde_json::Value::Array(rows)).unwrap_or_else(|_| "[]".to_string())
}

/// Renders `result` as a batch of `INSERT INTO table_name (...) VALUES (...);`
/// statements, quoting identifiers and literals the way `dialect` expects.
///
/// This is meant for human/portable export (copy into another tool, a
/// migration, a bug report), not as the mechanism used by the in-app
/// import flow, which binds parameters through the adapter instead of
/// building raw SQL text.
pub fn to_sql_inserts(result: &QueryResult, table_name: &str, dialect: DriverKind) -> String {
    let quoted_table = quote_ident(table_name, dialect);
    let quoted_columns: Vec<String> = result
        .columns
        .iter()
        .map(|c| quote_ident(&c.name, dialect))
        .collect();
    let columns_clause = quoted_columns.join(", ");

    let mut out = String::new();
    for row in &result.rows {
        let values: Vec<String> = row.iter().map(render_sql_literal).collect();
        let _ = writeln!(
            out,
            "INSERT INTO {quoted_table} ({columns_clause}) VALUES ({});",
            values.join(", ")
        );
    }
    out
}

fn quote_ident(ident: &str, dialect: DriverKind) -> String {
    match dialect {
        DriverKind::Postgres => format!("\"{}\"", ident.replace('"', "\"\"")),
        DriverKind::MySql => format!("`{}`", ident.replace('`', "``")),
        DriverKind::MsSql => format!("[{}]", ident.replace(']', "]]")),
    }
}

fn render_sql_literal(value: &Value) -> String {
    match value {
        Value::Null => "NULL".to_string(),
        Value::Bool(b) => b.to_string(),
        Value::I64(i) => i.to_string(),
        Value::F64(f) => f.to_string(),
        Value::Decimal(d) => d.to_string(),
        Value::Text(s) => format!("'{}'", s.replace('\'', "''")),
        Value::Bytes(b) => format!("'\\x{}'", b.iter().map(|x| format!("{x:02x}")).collect::<String>()),
        Value::Date(d) => format!("'{d}'"),
        Value::Time(t) => format!("'{t}'"),
        Value::DateTime(dt) => format!("'{dt}'"),
        Value::TimestampTz(dt) => format!("'{}'", dt.to_rfc3339()),
        Value::Uuid(u) => format!("'{u}'"),
        Value::Json(j) => format!("'{}'", j.to_string().replace('\'', "''")),
        Value::Array(items) => {
            let inner: Vec<String> = items.iter().map(render_sql_literal).collect();
            format!("ARRAY[{}]", inner.join(", "))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::result::Column;

    fn sample_result() -> QueryResult {
        QueryResult {
            columns: vec![
                Column { name: "id".into(), type_name: "int4".into(), nullable: false },
                Column { name: "name".into(), type_name: "text".into(), nullable: true },
            ],
            rows: vec![
                vec![Value::I64(1), Value::Text("Ada's".into())],
                vec![Value::I64(2), Value::Null],
            ],
            rows_affected: None,
            duration: Default::default(),
        }
    }

    #[test]
    fn to_csv_quotes_values_containing_commas_and_quotes() {
        let csv = to_csv(&sample_result()).expect("csv export should succeed");
        assert!(csv.contains("id,name"));
        assert!(csv.contains("Ada's"));
    }

    #[test]
    fn to_json_uses_column_names_as_keys() {
        let json = to_json(&sample_result());
        let parsed: serde_json::Value = serde_json::from_str(&json).expect("valid json");
        assert_eq!(parsed[0]["name"], serde_json::json!("Ada's"));
        assert_eq!(parsed[1]["name"], serde_json::Value::Null);
    }

    #[test]
    fn to_sql_inserts_escapes_single_quotes_for_postgres() {
        let sql = to_sql_inserts(&sample_result(), "users", DriverKind::Postgres);
        assert!(sql.contains("'Ada''s'"));
        assert!(sql.contains("INSERT INTO \"users\""));
    }
}
