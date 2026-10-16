use db_core::catalog::{
    Catalog, ColumnInfo, DatabaseInfo, ForeignKeyInfo, IndexInfo, SchemaInfo, TableInfo, TableKind,
};
use db_core::error::{DbError, Result};
use sqlx::mysql::MySqlConnection;
use sqlx::Row as _;
use std::collections::HashMap;

const SYSTEM_SCHEMAS: &[&str] = &["information_schema", "performance_schema", "mysql", "sys"];

/// MySQL has no schema namespace below a database (a "schema" and a
/// "database" are the same thing), so each accessible database becomes
/// both the [`DatabaseInfo`] and its single [`SchemaInfo`] — the `app`
/// crate's sidebar collapses that redundant level for this driver.
pub(crate) async fn fetch_catalog(conn: &mut MySqlConnection) -> Result<Catalog> {
    let db_names: Vec<String> = sqlx::query_scalar(
        "SELECT CAST(schema_name AS CHAR) FROM information_schema.schemata ORDER BY schema_name",
    )
    .fetch_all(&mut *conn)
    .await
    .map_err(|e| DbError::Introspection(e.to_string()))?
    .into_iter()
    .filter(|name: &String| !SYSTEM_SCHEMAS.contains(&name.as_str()))
    .collect();

    let mut databases = Vec::with_capacity(db_names.len());
    for db_name in db_names {
        let schema = fetch_schema(conn, &db_name).await?;
        databases.push(DatabaseInfo { name: db_name, schemas: vec![schema] });
    }

    Ok(Catalog { databases })
}

async fn fetch_schema(conn: &mut MySqlConnection, schema_name: &str) -> Result<SchemaInfo> {
    let table_rows = sqlx::query(
        "SELECT CAST(table_name AS CHAR), CAST(table_type AS CHAR) FROM information_schema.tables \
         WHERE table_schema = ? ORDER BY table_name",
    )
    .bind(schema_name)
    .fetch_all(&mut *conn)
    .await
    .map_err(|e| DbError::Introspection(e.to_string()))?;

    let mut tables = Vec::with_capacity(table_rows.len());
    for row in table_rows {
        let table_name: String = row.try_get(0).map_err(|e| DbError::Introspection(e.to_string()))?;
        let table_type: String = row.try_get(1).map_err(|e| DbError::Introspection(e.to_string()))?;
        let kind = if table_type == "VIEW" { TableKind::View } else { TableKind::Table };
        tables.push(fetch_table(conn, schema_name, &table_name, kind).await?);
    }

    Ok(SchemaInfo { name: schema_name.to_string(), tables, functions: Vec::new() })
}

struct KeyUsageRow {
    column_name: String,
    constraint_name: String,
    referenced_table: Option<String>,
    referenced_column: Option<String>,
}

async fn fetch_key_usage(
    conn: &mut MySqlConnection,
    schema_name: &str,
    table_name: &str,
) -> Result<Vec<KeyUsageRow>> {
    let rows = sqlx::query(
        "SELECT CAST(column_name AS CHAR), CAST(constraint_name AS CHAR), CAST(referenced_table_name AS CHAR), CAST(referenced_column_name AS CHAR) \
         FROM information_schema.key_column_usage \
         WHERE table_schema = ? AND table_name = ? \
         ORDER BY constraint_name, ordinal_position",
    )
    .bind(schema_name)
    .bind(table_name)
    .fetch_all(&mut *conn)
    .await
    .map_err(|e| DbError::Introspection(e.to_string()))?;

    rows.into_iter()
        .map(|row| {
            Ok(KeyUsageRow {
                column_name: row.try_get(0).map_err(|e| DbError::Introspection(e.to_string()))?,
                constraint_name: row.try_get(1).map_err(|e| DbError::Introspection(e.to_string()))?,
                referenced_table: row.try_get(2).map_err(|e| DbError::Introspection(e.to_string()))?,
                referenced_column: row.try_get(3).map_err(|e| DbError::Introspection(e.to_string()))?,
            })
        })
        .collect()
}

async fn fetch_table(
    conn: &mut MySqlConnection,
    schema_name: &str,
    table_name: &str,
    kind: TableKind,
) -> Result<TableInfo> {
    let key_usage = fetch_key_usage(conn, schema_name, table_name).await?;
    let primary_key: Vec<String> = key_usage
        .iter()
        .filter(|r| r.constraint_name == "PRIMARY")
        .map(|r| r.column_name.clone())
        .collect();
    let pk_set: std::collections::HashSet<&str> = primary_key.iter().map(String::as_str).collect();

    let column_rows = sqlx::query(
        "SELECT CAST(column_name AS CHAR), CAST(data_type AS CHAR), CAST(is_nullable AS CHAR), CAST(column_default AS CHAR), ordinal_position \
         FROM information_schema.columns WHERE table_schema = ? AND table_name = ? \
         ORDER BY ordinal_position",
    )
    .bind(schema_name)
    .bind(table_name)
    .fetch_all(&mut *conn)
    .await
    .map_err(|e| DbError::Introspection(e.to_string()))?;

    let mut columns = Vec::with_capacity(column_rows.len());
    for row in column_rows {
        let name: String = row.try_get(0).map_err(|e| DbError::Introspection(e.to_string()))?;
        let data_type: String = row.try_get(1).map_err(|e| DbError::Introspection(e.to_string()))?;
        let is_nullable: String = row.try_get(2).map_err(|e| DbError::Introspection(e.to_string()))?;
        let default: Option<String> =
            row.try_get(3).map_err(|e| DbError::Introspection(e.to_string()))?;
        let ordinal: u32 = row.try_get(4).map_err(|e| DbError::Introspection(e.to_string()))?;
        columns.push(ColumnInfo {
            is_primary_key: pk_set.contains(name.as_str()),
            name,
            data_type,
            nullable: is_nullable == "YES",
            default,
            ordinal: ordinal as i32,
        });
    }

    let foreign_keys = group_foreign_keys(&key_usage);
    let indexes = fetch_indexes(conn, schema_name, table_name).await?;

    Ok(TableInfo {
        name: table_name.to_string(),
        kind,
        columns,
        primary_key,
        foreign_keys,
        indexes,
        estimated_row_count: None,
    })
}

fn group_foreign_keys(key_usage: &[KeyUsageRow]) -> Vec<ForeignKeyInfo> {
    let mut by_name: HashMap<String, ForeignKeyInfo> = HashMap::new();
    let mut order: Vec<String> = Vec::new();

    for row in key_usage {
        let Some(referenced_table) = &row.referenced_table else { continue };
        let Some(referenced_column) = &row.referenced_column else { continue };

        let entry = by_name.entry(row.constraint_name.clone()).or_insert_with(|| {
            order.push(row.constraint_name.clone());
            ForeignKeyInfo {
                name: row.constraint_name.clone(),
                columns: Vec::new(),
                referenced_table: referenced_table.clone(),
                referenced_columns: Vec::new(),
            }
        });
        entry.columns.push(row.column_name.clone());
        entry.referenced_columns.push(referenced_column.clone());
    }

    order.into_iter().filter_map(|name| by_name.remove(&name)).collect()
}

async fn fetch_indexes(conn: &mut MySqlConnection, schema_name: &str, table_name: &str) -> Result<Vec<IndexInfo>> {
    let rows = sqlx::query(
        "SELECT CAST(index_name AS CHAR), non_unique, CAST(column_name AS CHAR) FROM information_schema.statistics \
         WHERE table_schema = ? AND table_name = ? ORDER BY index_name, seq_in_index",
    )
    .bind(schema_name)
    .bind(table_name)
    .fetch_all(&mut *conn)
    .await
    .map_err(|e| DbError::Introspection(e.to_string()))?;

    let mut by_name: HashMap<String, IndexInfo> = HashMap::new();
    let mut order: Vec<String> = Vec::new();
    for row in rows {
        let index_name: String = row.try_get(0).map_err(|e| DbError::Introspection(e.to_string()))?;
        let non_unique: i64 = row.try_get(1).map_err(|e| DbError::Introspection(e.to_string()))?;
        let column_name: String = row.try_get(2).map_err(|e| DbError::Introspection(e.to_string()))?;

        let entry = by_name.entry(index_name.clone()).or_insert_with(|| {
            order.push(index_name.clone());
            IndexInfo { name: index_name, columns: Vec::new(), unique: non_unique == 0 }
        });
        entry.columns.push(column_name);
    }

    Ok(order.into_iter().filter_map(|name| by_name.remove(&name)).collect())
}
