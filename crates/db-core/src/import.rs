//! Parsing CSV/JSON files into rows of [`Value`], ready to be handed to
//! [`crate::adapter::DbAdapter::bulk_insert`].
//!
//! Import is intentionally two-phase: parsing (here, pure and
//! backend-agnostic) and insertion (adapter-specific, parameter-bound —
//! never by building raw SQL text, to avoid injection on untrusted file
//! contents).

use crate::catalog::ColumnInfo;
use crate::error::{DbError, Result};
use crate::value::Value;
use chrono::{NaiveDate, NaiveDateTime, NaiveTime};
use rust_decimal::Decimal;
use std::str::FromStr;
use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct ImportBatch {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Value>>,
}

/// Parses `csv_text` using its header row as column names. Every cell is
/// coerced against the matching entry in `target_columns` (matched by
/// name, case-insensitively); columns present in the file but not in the
/// target table are rejected rather than silently dropped.
pub fn parse_csv(csv_text: &str, target_columns: &[ColumnInfo]) -> Result<ImportBatch> {
    let mut reader = csv::ReaderBuilder::new().from_reader(csv_text.as_bytes());
    let headers: Vec<String> = reader
        .headers()
        .map_err(|e| DbError::Io(e.to_string()))?
        .iter()
        .map(str::to_string)
        .collect();

    let column_types = resolve_column_types(&headers, target_columns)?;

    let mut rows = Vec::new();
    for record in reader.records() {
        let record = record.map_err(|e| DbError::Io(e.to_string()))?;
        let mut row = Vec::with_capacity(headers.len());
        for (cell, data_type) in record.iter().zip(column_types.iter()) {
            row.push(coerce_to_column_type(cell, data_type)?);
        }
        rows.push(row);
    }

    Ok(ImportBatch { columns: headers, rows })
}

/// Parses `json_text` as a JSON array of flat objects (the same shape
/// produced by [`crate::export::to_json`]). All objects must share the
/// same set of keys as the first one.
pub fn parse_json(json_text: &str, target_columns: &[ColumnInfo]) -> Result<ImportBatch> {
    let parsed: serde_json::Value =
        serde_json::from_str(json_text).map_err(|e| DbError::Io(e.to_string()))?;
    let array = parsed
        .as_array()
        .ok_or_else(|| DbError::Io("expected a JSON array of row objects".to_string()))?;

    let Some(first) = array.first() else {
        return Ok(ImportBatch { columns: Vec::new(), rows: Vec::new() });
    };
    let headers: Vec<String> = first
        .as_object()
        .ok_or_else(|| DbError::Io("expected each row to be a JSON object".to_string()))?
        .keys()
        .cloned()
        .collect();

    let column_types = resolve_column_types(&headers, target_columns)?;

    let mut rows = Vec::with_capacity(array.len());
    for item in array {
        let obj = item
            .as_object()
            .ok_or_else(|| DbError::Io("expected each row to be a JSON object".to_string()))?;
        let mut row = Vec::with_capacity(headers.len());
        for (header, data_type) in headers.iter().zip(column_types.iter()) {
            let cell = obj.get(header).unwrap_or(&serde_json::Value::Null);
            row.push(coerce_json_to_column_type(cell, data_type)?);
        }
        rows.push(row);
    }

    Ok(ImportBatch { columns: headers, rows })
}

fn resolve_column_types(headers: &[String], target_columns: &[ColumnInfo]) -> Result<Vec<String>> {
    headers
        .iter()
        .map(|h| {
            target_columns
                .iter()
                .find(|c| c.name.eq_ignore_ascii_case(h))
                .map(|c| c.data_type.clone())
                .ok_or_else(|| DbError::Io(format!("column `{h}` does not exist on the target table")))
        })
        .collect()
}

/// Best-effort coercion of a raw CSV cell into a typed [`Value`], guided
/// by the target column's catalog `data_type` string (e.g. `int4`,
/// `varchar`, `bigint`, `timestamp`). Unknown/unrecognized type names fall
/// back to `Value::Text`, which every adapter accepts and lets the
/// server-side cast handle.
fn coerce_to_column_type(raw: &str, data_type: &str) -> Result<Value> {
    if raw.is_empty() {
        return Ok(Value::Null);
    }
    let ty = data_type.to_ascii_lowercase();

    if ty.contains("bool") {
        return parse_bool(raw).map(Value::Bool);
    }
    if ty.contains("int") {
        return raw
            .parse::<i64>()
            .map(Value::I64)
            .map_err(|_| DbError::Conversion(format!("`{raw}` is not a valid integer for column type `{data_type}`")));
    }
    if ty.contains("numeric") || ty.contains("decimal") {
        return Decimal::from_str(raw)
            .map(Value::Decimal)
            .map_err(|_| DbError::Conversion(format!("`{raw}` is not a valid decimal for column type `{data_type}`")));
    }
    if ty.contains("float") || ty.contains("double") || ty.contains("real") {
        return raw
            .parse::<f64>()
            .map(Value::F64)
            .map_err(|_| DbError::Conversion(format!("`{raw}` is not a valid float for column type `{data_type}`")));
    }
    if ty.contains("uuid") || ty.contains("uniqueidentifier") {
        return Uuid::parse_str(raw)
            .map(Value::Uuid)
            .map_err(|_| DbError::Conversion(format!("`{raw}` is not a valid UUID for column type `{data_type}`")));
    }
    if ty.contains("json") {
        return serde_json::from_str(raw)
            .map(Value::Json)
            .map_err(|_| DbError::Conversion(format!("`{raw}` is not valid JSON for column type `{data_type}`")));
    }
    if ty == "date" {
        return NaiveDate::parse_from_str(raw, "%Y-%m-%d")
            .map(Value::Date)
            .map_err(|_| DbError::Conversion(format!("`{raw}` is not a valid date (expected YYYY-MM-DD)")));
    }
    if ty.contains("time") && !ty.contains("stamp") && !ty.contains("datetime") {
        return NaiveTime::parse_from_str(raw, "%H:%M:%S")
            .map(Value::Time)
            .map_err(|_| DbError::Conversion(format!("`{raw}` is not a valid time (expected HH:MM:SS)")));
    }
    if ty.contains("timestamp") || ty.contains("datetime") {
        return NaiveDateTime::parse_from_str(raw, "%Y-%m-%d %H:%M:%S")
            .or_else(|_| NaiveDateTime::parse_from_str(raw, "%Y-%m-%dT%H:%M:%S"))
            .map(Value::DateTime)
            .map_err(|_| DbError::Conversion(format!("`{raw}` is not a valid timestamp")));
    }

    Ok(Value::Text(raw.to_string()))
}

fn coerce_json_to_column_type(cell: &serde_json::Value, data_type: &str) -> Result<Value> {
    match cell {
        serde_json::Value::Null => Ok(Value::Null),
        serde_json::Value::Bool(b) => Ok(Value::Bool(*b)),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Ok(Value::I64(i))
            } else {
                Ok(Value::F64(n.as_f64().unwrap_or_default()))
            }
        }
        serde_json::Value::String(s) => coerce_to_column_type(s, data_type),
        other => Ok(Value::Json(other.clone())),
    }
}

fn parse_bool(raw: &str) -> Result<bool> {
    match raw.to_ascii_lowercase().as_str() {
        "true" | "t" | "1" | "yes" => Ok(true),
        "false" | "f" | "0" | "no" => Ok(false),
        _ => Err(DbError::Conversion(format!("`{raw}` is not a valid boolean"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::ColumnInfo;

    fn columns() -> Vec<ColumnInfo> {
        vec![
            ColumnInfo {
                name: "id".into(),
                data_type: "int4".into(),
                nullable: false,
                default: None,
                is_primary_key: true,
                ordinal: 0,
            },
            ColumnInfo {
                name: "name".into(),
                data_type: "text".into(),
                nullable: true,
                default: None,
                is_primary_key: false,
                ordinal: 1,
            },
        ]
    }

    #[test]
    fn parse_csv_coerces_integers_and_passes_through_text() {
        let csv = "id,name\n1,Ada\n2,\n";
        let batch = parse_csv(csv, &columns()).expect("csv should parse");
        assert_eq!(batch.rows[0], vec![Value::I64(1), Value::Text("Ada".into())]);
        assert_eq!(batch.rows[1], vec![Value::I64(2), Value::Null]);
    }

    #[test]
    fn parse_csv_rejects_unknown_columns() {
        let csv = "id,not_a_column\n1,x\n";
        let err = parse_csv(csv, &columns()).unwrap_err();
        assert!(matches!(err, DbError::Io(_)));
    }

    #[test]
    fn parse_json_coerces_matching_shapes() {
        let json = r#"[{"id": 1, "name": "Ada"}, {"id": 2, "name": null}]"#;
        let batch = parse_json(json, &columns()).expect("json should parse");
        assert_eq!(batch.rows[0], vec![Value::I64(1), Value::Text("Ada".into())]);
        assert_eq!(batch.rows[1][1], Value::Null);
    }
}
