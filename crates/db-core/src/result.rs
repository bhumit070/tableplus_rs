use crate::value::Value;
use std::time::Duration;

/// Metadata for one column of a result set.
#[derive(Debug, Clone, PartialEq)]
pub struct Column {
    pub name: String,
    /// The backend's native type name (e.g. `int4`, `varchar`, `BIGINT`),
    /// kept as-is for display in the UI and for autocomplete hints.
    pub type_name: String,
    pub nullable: bool,
}

/// The outcome of running one statement.
#[derive(Debug, Clone, Default)]
pub struct QueryResult {
    pub columns: Vec<Column>,
    pub rows: Vec<Vec<Value>>,
    /// `Some(n)` for statements that don't return rows (INSERT/UPDATE/DELETE/DDL).
    pub rows_affected: Option<u64>,
    pub duration: Duration,
}

impl QueryResult {
    pub fn empty() -> Self {
        Self::default()
    }

    pub fn row_count(&self) -> usize {
        self.rows.len()
    }

    pub fn column_index(&self, name: &str) -> Option<usize> {
        self.columns.iter().position(|c| c.name == name)
    }
}
