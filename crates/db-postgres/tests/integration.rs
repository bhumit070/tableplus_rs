//! Integration tests against the real Postgres container started by the
//! repo-root `docker-compose.yml`. Run `docker compose up -d` first; see
//! DEVELOPMENT.md for details. Connection settings can be overridden with
//! `TABLEPLUS_RS_POSTGRES_{HOST,PORT,USER,PASSWORD,DATABASE}`.

use db_core::adapter::DbAdapter;
use db_core::connection::{ConnectionConfig, DriverKind};
use db_core::value::Value;

fn test_config() -> (ConnectionConfig, String) {
    let mut config = ConnectionConfig::new("integration-test", DriverKind::Postgres);
    config.host = std::env::var("TABLEPLUS_RS_POSTGRES_HOST").unwrap_or_else(|_| "localhost".into());
    config.port = std::env::var("TABLEPLUS_RS_POSTGRES_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(55432);
    config.username = std::env::var("TABLEPLUS_RS_POSTGRES_USER").unwrap_or_else(|_| "tableplus".into());
    config.database =
        Some(std::env::var("TABLEPLUS_RS_POSTGRES_DATABASE").unwrap_or_else(|_| "tableplus_rs_sample".into()));
    let password = std::env::var("TABLEPLUS_RS_POSTGRES_PASSWORD").unwrap_or_else(|_| "tableplus".into());
    (config, password)
}

#[tokio::test]
async fn connect_and_select_literal_roundtrips_value() {
    let (config, password) = test_config();
    let adapter = db_postgres::connect(&config, &password)
        .await
        .expect("should connect to the docker-compose postgres container");

    let result = adapter.execute("SELECT 1 AS answer").await.expect("query should succeed");
    assert_eq!(result.columns[0].name, "answer");
    assert_eq!(result.rows[0][0], Value::I64(1));
}

#[tokio::test]
async fn fetch_catalog_finds_sample_shop_schema_and_tables() {
    let (config, password) = test_config();
    let adapter = db_postgres::connect(&config, &password).await.expect("should connect");

    let catalog = adapter.fetch_catalog().await.expect("introspection should succeed");
    let shop_schema = catalog.databases[0]
        .schemas
        .iter()
        .find(|s| s.name == "shop")
        .expect("sample schema `shop` should exist");

    let customers = shop_schema
        .tables
        .iter()
        .find(|t| t.name == "customers")
        .expect("sample table `customers` should exist");
    assert_eq!(customers.primary_key, vec!["id".to_string()]);
    assert!(customers.columns.iter().any(|c| c.name == "email"));

    let orders = shop_schema.tables.iter().find(|t| t.name == "orders").expect("orders table should exist");
    assert_eq!(orders.foreign_keys.len(), 1);
    assert_eq!(orders.foreign_keys[0].referenced_table, "customers");
}

#[tokio::test]
async fn bulk_insert_then_select_reads_back_inserted_rows() {
    let (config, password) = test_config();
    let adapter = db_postgres::connect(&config, &password).await.expect("should connect");

    let columns = vec!["full_name".to_string(), "email".to_string(), "is_active".to_string()];
    let unique_email = format!("bulk-{}@example.com", uuid::Uuid::new_v4());
    let rows = vec![vec![
        Value::Text("Bulk Insert Test".to_string()),
        Value::Text(unique_email.clone()),
        Value::Bool(true),
    ]];

    let inserted = adapter
        .bulk_insert("shop.customers", &columns, &rows)
        .await
        .expect("bulk insert should succeed");
    assert_eq!(inserted, 1);

    let result = adapter
        .execute(&format!("SELECT full_name FROM shop.customers WHERE email = '{unique_email}'"))
        .await
        .expect("select should succeed");
    assert_eq!(result.rows.len(), 1);
    assert_eq!(result.rows[0][0], Value::Text("Bulk Insert Test".to_string()));
}

/// A real-world schema inevitably has column types we don't special-case
/// (custom enums, arrays of types we don't special-case, `inet`, ranges,
/// composites, ...). sqlx rejects decoding those as `String` outright, so
/// a query touching one used to fail entirely instead of just that one
/// cell. This pins the fix: `execute` must never error over a single
/// unrecognized column.
#[tokio::test]
async fn execute_degrades_unrecognized_column_types_instead_of_failing_the_query() {
    let (config, password) = test_config();
    let adapter = db_postgres::connect(&config, &password).await.expect("should connect");

    adapter.execute("DROP TABLE IF EXISTS exotic_types").await.ok();
    adapter.execute("DROP TYPE IF EXISTS mood").await.ok();
    adapter.execute("CREATE TYPE mood AS ENUM ('happy', 'sad')").await.expect("create enum type");
    adapter
        .execute("CREATE TABLE exotic_types (id INT, current_mood mood, tags UUID[], address INET)")
        .await
        .expect("create table with exotic column types");
    adapter
        .execute(
            "INSERT INTO exotic_types VALUES \
             (1, 'happy', ARRAY[gen_random_uuid()], '127.0.0.1')",
        )
        .await
        .expect("insert should succeed");

    let result = adapter
        .execute("SELECT id, current_mood, tags, address FROM exotic_types")
        .await
        .expect("select must not fail just because some columns are unrecognized types");

    assert_eq!(result.rows.len(), 1);
    assert_eq!(result.rows[0][0], Value::I64(1));
    // Whatever these decode to, they must not be an error that took the
    // whole query down with them — every cell comes back as *something*.
    for cell in &result.rows[0][1..] {
        assert!(!matches!(cell, Value::Null), "exotic columns have real values, not NULL");
    }

    adapter.execute("DROP TABLE exotic_types").await.ok();
    adapter.execute("DROP TYPE mood").await.ok();
}
