use chrono::{DateTime, NaiveDate, NaiveDateTime, NaiveTime, Utc};
use rust_decimal::Decimal;
use std::fmt;
use uuid::Uuid;

/// A single cell value, normalized across Postgres/MySQL/MSSQL wire types.
///
/// Every adapter is responsible for converting its driver's native row
/// representation into this enum. The GUI, the SQL editor, and the
/// export/import code only ever operate on `Value`, so they stay fully
/// decoupled from which database produced the data.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    I64(i64),
    F64(f64),
    Decimal(Decimal),
    Text(String),
    Bytes(Vec<u8>),
    Date(NaiveDate),
    Time(NaiveTime),
    DateTime(NaiveDateTime),
    TimestampTz(DateTime<Utc>),
    Uuid(Uuid),
    Json(serde_json::Value),
    /// Postgres-style arrays (or any multi-valued column).
    Array(Vec<Value>),
}

impl Value {
    pub fn is_null(&self) -> bool {
        matches!(self, Value::Null)
    }

    /// Renders the value the way a human expects to see it in a data grid
    /// cell. `NULL` is rendered as the literal string `"NULL"` by callers
    /// that want to distinguish it visually, not here.
    pub fn display(&self) -> String {
        match self {
            Value::Null => String::new(),
            Value::Bool(b) => b.to_string(),
            Value::I64(i) => i.to_string(),
            Value::F64(f) => f.to_string(),
            Value::Decimal(d) => d.to_string(),
            Value::Text(s) => s.clone(),
            Value::Bytes(b) => format!("0x{}", hex_encode(b)),
            Value::Date(d) => d.to_string(),
            Value::Time(t) => t.to_string(),
            Value::DateTime(dt) => dt.to_string(),
            Value::TimestampTz(dt) => dt.to_rfc3339(),
            Value::Uuid(u) => u.to_string(),
            Value::Json(j) => j.to_string(),
            Value::Array(items) => {
                let inner: Vec<String> = items.iter().map(Value::display).collect();
                format!("[{}]", inner.join(", "))
            }
        }
    }

    /// Lossless-ish conversion to `serde_json::Value`, used by JSON export
    /// and by anything that needs a serializable representation.
    pub fn to_json(&self) -> serde_json::Value {
        use serde_json::Value as J;
        match self {
            Value::Null => J::Null,
            Value::Bool(b) => J::Bool(*b),
            Value::I64(i) => J::Number((*i).into()),
            Value::F64(f) => serde_json::Number::from_f64(*f).map(J::Number).unwrap_or(J::Null),
            Value::Decimal(d) => J::String(d.to_string()),
            Value::Text(s) => J::String(s.clone()),
            Value::Bytes(b) => J::String(format!("0x{}", hex_encode(b))),
            Value::Date(d) => J::String(d.to_string()),
            Value::Time(t) => J::String(t.to_string()),
            Value::DateTime(dt) => J::String(dt.to_string()),
            Value::TimestampTz(dt) => J::String(dt.to_rfc3339()),
            Value::Uuid(u) => J::String(u.to_string()),
            Value::Json(j) => j.clone(),
            Value::Array(items) => J::Array(items.iter().map(Value::to_json).collect()),
        }
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.display())
    }
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
