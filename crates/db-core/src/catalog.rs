//! Normalized schema metadata.
//!
//! Every adapter fills in a [`Catalog`] by introspecting the server
//! (`information_schema` on Postgres/MySQL/MSSQL, falling back to
//! system catalogs where `information_schema` is incomplete). The
//! sidebar tree, the autocomplete engine in `sql-intel`, and the
//! export/import tooling all consume this same shape, so none of them
//! need to know which backend produced it.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Catalog {
    pub databases: Vec<DatabaseInfo>,
}

impl Catalog {
    /// Flat iterator over every table/view in the catalog, convenient for
    /// autocomplete and fuzzy search across the whole connection.
    pub fn all_tables(&self) -> impl Iterator<Item = (&DatabaseInfo, &SchemaInfo, &TableInfo)> {
        self.databases.iter().flat_map(|db| {
            db.schemas
                .iter()
                .flat_map(move |schema| schema.tables.iter().map(move |t| (db, schema, t)))
        })
    }

    pub fn find_table(&self, name: &str) -> Option<&TableInfo> {
        self.all_tables()
            .find(|(_, _, t)| t.name.eq_ignore_ascii_case(name))
            .map(|(_, _, t)| t)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DatabaseInfo {
    pub name: String,
    pub schemas: Vec<SchemaInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SchemaInfo {
    pub name: String,
    pub tables: Vec<TableInfo>,
    #[serde(default)]
    pub functions: Vec<FunctionInfo>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TableKind {
    Table,
    View,
    MaterializedView,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TableInfo {
    pub name: String,
    pub kind: TableKind,
    pub columns: Vec<ColumnInfo>,
    pub primary_key: Vec<String>,
    #[serde(default)]
    pub foreign_keys: Vec<ForeignKeyInfo>,
    #[serde(default)]
    pub indexes: Vec<IndexInfo>,
    pub estimated_row_count: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ColumnInfo {
    pub name: String,
    pub data_type: String,
    pub nullable: bool,
    pub default: Option<String>,
    pub is_primary_key: bool,
    pub ordinal: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ForeignKeyInfo {
    pub name: String,
    pub columns: Vec<String>,
    pub referenced_table: String,
    pub referenced_columns: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexInfo {
    pub name: String,
    pub columns: Vec<String>,
    pub unique: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionInfo {
    pub name: String,
    pub signature: String,
}
