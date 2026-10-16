use db_core::catalog::{
    Catalog, ColumnInfo, DatabaseInfo, ForeignKeyInfo, IndexInfo, SchemaInfo, TableInfo, TableKind,
};
use db_core::error::{DbError, Result};
use sqlx::postgres::PgConnection;
use sqlx::Row as _;
use std::collections::HashMap;

pub(crate) async fn fetch_catalog(conn: &mut PgConnection) -> Result<Catalog> {
    let db_name: String = sqlx::query_scalar("SELECT current_database()")
        .fetch_one(&mut *conn)
        .await
        .map_err(|e| DbError::Introspection(e.to_string()))?;

    let schema_names: Vec<String> = sqlx::query_scalar(
        "SELECT schema_name FROM information_schema.schemata \
         WHERE schema_name NOT IN ('pg_catalog', 'information_schema') \
         AND schema_name NOT LIKE 'pg_toast%' AND schema_name NOT LIKE 'pg_temp%' \
         ORDER BY schema_name",
    )
    .fetch_all(&mut *conn)
    .await
    .map_err(|e| DbError::Introspection(e.to_string()))?;

    let mut schemas = Vec::with_capacity(schema_names.len());
    for schema_name in schema_names {
        schemas.push(fetch_schema(conn, &schema_name).await?);
    }

    Ok(Catalog { databases: vec![DatabaseInfo { name: db_name, schemas }] })
}

async fn fetch_schema(conn: &mut PgConnection, schema_name: &str) -> Result<SchemaInfo> {
    let table_rows = sqlx::query(
        "SELECT table_name, table_type FROM information_schema.tables \
         WHERE table_schema = $1 ORDER BY table_name",
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

async fn fetch_table(
    conn: &mut PgConnection,
    schema_name: &str,
    table_name: &str,
    kind: TableKind,
) -> Result<TableInfo> {
    let primary_key = fetch_primary_key(conn, schema_name, table_name).await?;
    let pk_set: std::collections::HashSet<&str> = primary_key.iter().map(String::as_str).collect();

    let column_rows = sqlx::query(
        "SELECT column_name, data_type, is_nullable, column_default, ordinal_position \
         FROM information_schema.columns WHERE table_schema = $1 AND table_name = $2 \
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
        let ordinal: i32 = row.try_get(4).map_err(|e| DbError::Introspection(e.to_string()))?;
        columns.push(ColumnInfo {
            is_primary_key: pk_set.contains(name.as_str()),
            name,
            data_type,
            nullable: is_nullable == "YES",
            default,
            ordinal,
        });
    }

    let foreign_keys = fetch_foreign_keys(conn, schema_name, table_name).await?;
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

async fn fetch_primary_key(conn: &mut PgConnection, schema_name: &str, table_name: &str) -> Result<Vec<String>> {
    sqlx::query_scalar(
        "SELECT kcu.column_name FROM information_schema.table_constraints tc \
         JOIN information_schema.key_column_usage kcu \
           ON tc.constraint_name = kcu.constraint_name AND tc.table_schema = kcu.table_schema \
         WHERE tc.constraint_type = 'PRIMARY KEY' AND tc.table_schema = $1 AND tc.table_name = $2 \
         ORDER BY kcu.ordinal_position",
    )
    .bind(schema_name)
    .bind(table_name)
    .fetch_all(&mut *conn)
    .await
    .map_err(|e| DbError::Introspection(e.to_string()))
}

async fn fetch_foreign_keys(
    conn: &mut PgConnection,
    schema_name: &str,
    table_name: &str,
) -> Result<Vec<ForeignKeyInfo>> {
    let rows = sqlx::query(
        "SELECT tc.constraint_name, kcu.column_name, ccu.table_name AS ref_table, ccu.column_name AS ref_column \
         FROM information_schema.table_constraints tc \
         JOIN information_schema.key_column_usage kcu \
           ON tc.constraint_name = kcu.constraint_name AND tc.table_schema = kcu.table_schema \
         JOIN information_schema.constraint_column_usage ccu \
           ON tc.constraint_name = ccu.constraint_name AND tc.table_schema = ccu.table_schema \
         WHERE tc.constraint_type = 'FOREIGN KEY' AND tc.table_schema = $1 AND tc.table_name = $2 \
         ORDER BY tc.constraint_name, kcu.ordinal_position",
    )
    .bind(schema_name)
    .bind(table_name)
    .fetch_all(&mut *conn)
    .await
    .map_err(|e| DbError::Introspection(e.to_string()))?;

    let mut by_name: HashMap<String, ForeignKeyInfo> = HashMap::new();
    let mut order: Vec<String> = Vec::new();
    for row in rows {
        let constraint_name: String = row.try_get(0).map_err(|e| DbError::Introspection(e.to_string()))?;
        let column: String = row.try_get(1).map_err(|e| DbError::Introspection(e.to_string()))?;
        let ref_table: String = row.try_get(2).map_err(|e| DbError::Introspection(e.to_string()))?;
        let ref_column: String = row.try_get(3).map_err(|e| DbError::Introspection(e.to_string()))?;

        let entry = by_name.entry(constraint_name.clone()).or_insert_with(|| {
            order.push(constraint_name.clone());
            ForeignKeyInfo {
                name: constraint_name,
                columns: Vec::new(),
                referenced_table: ref_table,
                referenced_columns: Vec::new(),
            }
        });
        entry.columns.push(column);
        entry.referenced_columns.push(ref_column);
    }

    Ok(order.into_iter().filter_map(|name| by_name.remove(&name)).collect())
}

async fn fetch_indexes(conn: &mut PgConnection, schema_name: &str, table_name: &str) -> Result<Vec<IndexInfo>> {
    let rows = sqlx::query(
        "SELECT i.relname AS index_name, ix.indisunique AS is_unique, a.attname AS column_name \
         FROM pg_class t \
         JOIN pg_index ix ON t.oid = ix.indrelid \
         JOIN pg_class i ON i.oid = ix.indexrelid \
         JOIN pg_namespace n ON n.oid = t.relnamespace \
         JOIN unnest(ix.indkey) WITH ORDINALITY AS k(attnum, ord) ON true \
         JOIN pg_attribute a ON a.attrelid = t.oid AND a.attnum = k.attnum \
         WHERE n.nspname = $1 AND t.relname = $2 \
         ORDER BY i.relname, k.ord",
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
        let is_unique: bool = row.try_get(1).map_err(|e| DbError::Introspection(e.to_string()))?;
        let column_name: String = row.try_get(2).map_err(|e| DbError::Introspection(e.to_string()))?;

        let entry = by_name.entry(index_name.clone()).or_insert_with(|| {
            order.push(index_name.clone());
            IndexInfo { name: index_name, columns: Vec::new(), unique: is_unique }
        });
        entry.columns.push(column_name);
    }

    Ok(order.into_iter().filter_map(|name| by_name.remove(&name)).collect())
}
