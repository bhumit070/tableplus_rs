use crate::TdsClient;
use db_core::catalog::{
    Catalog, ColumnInfo, DatabaseInfo, ForeignKeyInfo, IndexInfo, SchemaInfo, TableInfo, TableKind,
};
use db_core::error::{DbError, Result};
use std::collections::HashMap;

pub(crate) async fn fetch_catalog(client: &mut TdsClient) -> Result<Catalog> {
    let db_name = query_scalar_string(client, "SELECT DB_NAME()", &[]).await?;

    let schema_names = query_scalar_strings(
        client,
        "SELECT DISTINCT TABLE_SCHEMA FROM INFORMATION_SCHEMA.TABLES ORDER BY TABLE_SCHEMA",
        &[],
    )
    .await?;

    let mut schemas = Vec::with_capacity(schema_names.len());
    for schema_name in schema_names {
        schemas.push(fetch_schema(client, &schema_name).await?);
    }

    Ok(Catalog { databases: vec![DatabaseInfo { name: db_name, schemas }] })
}

async fn query_scalar_string(client: &mut TdsClient, sql: &str, params: &[&dyn tiberius::ToSql]) -> Result<String> {
    let row = client
        .query(sql, params)
        .await
        .map_err(|e| DbError::Introspection(e.to_string()))?
        .into_row()
        .await
        .map_err(|e| DbError::Introspection(e.to_string()))?
        .ok_or_else(|| DbError::Introspection(format!("query `{sql}` returned no rows")))?;
    Ok(row.get::<&str, _>(0).unwrap_or_default().to_string())
}

async fn query_scalar_strings(client: &mut TdsClient, sql: &str, params: &[&dyn tiberius::ToSql]) -> Result<Vec<String>> {
    let rows = client
        .query(sql, params)
        .await
        .map_err(|e| DbError::Introspection(e.to_string()))?
        .into_first_result()
        .await
        .map_err(|e| DbError::Introspection(e.to_string()))?;
    Ok(rows.iter().map(|r| r.get::<&str, _>(0).unwrap_or_default().to_string()).collect())
}

async fn fetch_schema(client: &mut TdsClient, schema_name: &str) -> Result<SchemaInfo> {
    let rows = client
        .query(
            "SELECT TABLE_NAME, TABLE_TYPE FROM INFORMATION_SCHEMA.TABLES \
             WHERE TABLE_SCHEMA = @P1 ORDER BY TABLE_NAME",
            &[&schema_name],
        )
        .await
        .map_err(|e| DbError::Introspection(e.to_string()))?
        .into_first_result()
        .await
        .map_err(|e| DbError::Introspection(e.to_string()))?;

    let mut tables = Vec::with_capacity(rows.len());
    for row in rows {
        let table_name = row.get::<&str, _>(0).unwrap_or_default().to_string();
        let table_type = row.get::<&str, _>(1).unwrap_or_default();
        let kind = if table_type == "VIEW" { TableKind::View } else { TableKind::Table };
        tables.push(fetch_table(client, schema_name, &table_name, kind).await?);
    }

    Ok(SchemaInfo { name: schema_name.to_string(), tables, functions: Vec::new() })
}

async fn fetch_table(
    client: &mut TdsClient,
    schema_name: &str,
    table_name: &str,
    kind: TableKind,
) -> Result<TableInfo> {
    let primary_key = fetch_primary_key(client, schema_name, table_name).await?;
    let pk_set: std::collections::HashSet<&str> = primary_key.iter().map(String::as_str).collect();

    let column_rows = client
        .query(
            "SELECT COLUMN_NAME, DATA_TYPE, IS_NULLABLE, COLUMN_DEFAULT, ORDINAL_POSITION \
             FROM INFORMATION_SCHEMA.COLUMNS WHERE TABLE_SCHEMA = @P1 AND TABLE_NAME = @P2 \
             ORDER BY ORDINAL_POSITION",
            &[&schema_name, &table_name],
        )
        .await
        .map_err(|e| DbError::Introspection(e.to_string()))?
        .into_first_result()
        .await
        .map_err(|e| DbError::Introspection(e.to_string()))?;

    let mut columns = Vec::with_capacity(column_rows.len());
    for row in column_rows {
        let name = row.get::<&str, _>(0).unwrap_or_default().to_string();
        let data_type = row.get::<&str, _>(1).unwrap_or_default().to_string();
        let is_nullable = row.get::<&str, _>(2).unwrap_or_default();
        let default = row.get::<&str, _>(3).map(str::to_string);
        let ordinal: i32 = row.get(4).unwrap_or_default();
        columns.push(ColumnInfo {
            is_primary_key: pk_set.contains(name.as_str()),
            name,
            data_type,
            nullable: is_nullable == "YES",
            default,
            ordinal,
        });
    }

    let foreign_keys = fetch_foreign_keys(client, schema_name, table_name).await?;
    let indexes = fetch_indexes(client, schema_name, table_name).await?;

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

async fn fetch_primary_key(client: &mut TdsClient, schema_name: &str, table_name: &str) -> Result<Vec<String>> {
    let rows = client
        .query(
            "SELECT ku.COLUMN_NAME FROM INFORMATION_SCHEMA.TABLE_CONSTRAINTS tc \
             JOIN INFORMATION_SCHEMA.KEY_COLUMN_USAGE ku \
               ON tc.CONSTRAINT_NAME = ku.CONSTRAINT_NAME AND tc.TABLE_SCHEMA = ku.TABLE_SCHEMA \
             WHERE tc.CONSTRAINT_TYPE = 'PRIMARY KEY' AND tc.TABLE_SCHEMA = @P1 AND tc.TABLE_NAME = @P2 \
             ORDER BY ku.ORDINAL_POSITION",
            &[&schema_name, &table_name],
        )
        .await
        .map_err(|e| DbError::Introspection(e.to_string()))?
        .into_first_result()
        .await
        .map_err(|e| DbError::Introspection(e.to_string()))?;
    Ok(rows.iter().map(|r| r.get::<&str, _>(0).unwrap_or_default().to_string()).collect())
}

async fn fetch_foreign_keys(
    client: &mut TdsClient,
    schema_name: &str,
    table_name: &str,
) -> Result<Vec<ForeignKeyInfo>> {
    let rows = client
        .query(
            "SELECT rc.CONSTRAINT_NAME, kcu1.COLUMN_NAME, kcu2.TABLE_NAME, kcu2.COLUMN_NAME \
             FROM INFORMATION_SCHEMA.REFERENTIAL_CONSTRAINTS rc \
             JOIN INFORMATION_SCHEMA.KEY_COLUMN_USAGE kcu1 \
               ON rc.CONSTRAINT_NAME = kcu1.CONSTRAINT_NAME AND rc.CONSTRAINT_SCHEMA = kcu1.TABLE_SCHEMA \
             JOIN INFORMATION_SCHEMA.KEY_COLUMN_USAGE kcu2 \
               ON rc.UNIQUE_CONSTRAINT_NAME = kcu2.CONSTRAINT_NAME \
               AND rc.UNIQUE_CONSTRAINT_SCHEMA = kcu2.TABLE_SCHEMA \
               AND kcu1.ORDINAL_POSITION = kcu2.ORDINAL_POSITION \
             WHERE kcu1.TABLE_SCHEMA = @P1 AND kcu1.TABLE_NAME = @P2 \
             ORDER BY rc.CONSTRAINT_NAME, kcu1.ORDINAL_POSITION",
            &[&schema_name, &table_name],
        )
        .await
        .map_err(|e| DbError::Introspection(e.to_string()))?
        .into_first_result()
        .await
        .map_err(|e| DbError::Introspection(e.to_string()))?;

    let mut by_name: HashMap<String, ForeignKeyInfo> = HashMap::new();
    let mut order: Vec<String> = Vec::new();
    for row in rows {
        let constraint_name = row.get::<&str, _>(0).unwrap_or_default().to_string();
        let column = row.get::<&str, _>(1).unwrap_or_default().to_string();
        let ref_table = row.get::<&str, _>(2).unwrap_or_default().to_string();
        let ref_column = row.get::<&str, _>(3).unwrap_or_default().to_string();

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

async fn fetch_indexes(client: &mut TdsClient, schema_name: &str, table_name: &str) -> Result<Vec<IndexInfo>> {
    let rows = client
        .query(
            "SELECT i.name, i.is_unique, c.name \
             FROM sys.indexes i \
             JOIN sys.index_columns ic ON i.object_id = ic.object_id AND i.index_id = ic.index_id \
             JOIN sys.columns c ON ic.object_id = c.object_id AND ic.column_id = c.column_id \
             JOIN sys.tables t ON i.object_id = t.object_id \
             JOIN sys.schemas s ON t.schema_id = s.schema_id \
             WHERE s.name = @P1 AND t.name = @P2 AND i.name IS NOT NULL \
             ORDER BY i.name, ic.key_ordinal",
            &[&schema_name, &table_name],
        )
        .await
        .map_err(|e| DbError::Introspection(e.to_string()))?
        .into_first_result()
        .await
        .map_err(|e| DbError::Introspection(e.to_string()))?;

    let mut by_name: HashMap<String, IndexInfo> = HashMap::new();
    let mut order: Vec<String> = Vec::new();
    for row in rows {
        let index_name = row.get::<&str, _>(0).unwrap_or_default().to_string();
        let is_unique: bool = row.get(1).unwrap_or_default();
        let column_name = row.get::<&str, _>(2).unwrap_or_default().to_string();

        let entry = by_name.entry(index_name.clone()).or_insert_with(|| {
            order.push(index_name.clone());
            IndexInfo { name: index_name, columns: Vec::new(), unique: is_unique }
        });
        entry.columns.push(column_name);
    }

    Ok(order.into_iter().filter_map(|name| by_name.remove(&name)).collect())
}
