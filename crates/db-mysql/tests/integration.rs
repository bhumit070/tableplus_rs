//! Integration tests against the real MySQL container started by the
//! repo-root `docker-compose.yml`. Run `docker compose up -d` first; see
//! DEVELOPMENT.md for details. Connection settings can be overridden with
//! `TABLEPLUS_RS_MYSQL_{HOST,PORT,USER,PASSWORD,DATABASE}`.

use db_core::adapter::DbAdapter;
use db_core::connection::{ConnectionConfig, DriverKind};
use db_core::value::Value;

fn test_config() -> (ConnectionConfig, String) {
    let mut config = ConnectionConfig::new("integration-test", DriverKind::MySql);
    config.host = std::env::var("TABLEPLUS_RS_MYSQL_HOST").unwrap_or_else(|_| "localhost".into());
    config.port = std::env::var("TABLEPLUS_RS_MYSQL_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(53306);
    config.username = std::env::var("TABLEPLUS_RS_MYSQL_USER").unwrap_or_else(|_| "tableplus".into());
    config.database =
        Some(std::env::var("TABLEPLUS_RS_MYSQL_DATABASE").unwrap_or_else(|_| "tableplus_rs_sample".into()));
    let password = std::env::var("TABLEPLUS_RS_MYSQL_PASSWORD").unwrap_or_else(|_| "tableplus".into());
    (config, password)
}

#[tokio::test]
async fn connect_and_select_literal_roundtrips_value() {
    let (config, password) = test_config();
    let adapter = db_mysql::connect(&config, &password)
        .await
        .expect("should connect to the docker-compose mysql container");

    let result = adapter.execute("SELECT 1 AS answer").await.expect("query should succeed");
    assert_eq!(result.columns[0].name, "answer");
    assert_eq!(result.rows[0][0], Value::I64(1));
}

#[tokio::test]
async fn fetch_catalog_finds_sample_tables() {
    let (config, password) = test_config();
    let adapter = db_mysql::connect(&config, &password).await.expect("should connect");

    let catalog = adapter.fetch_catalog().await.expect("introspection should succeed");
    let sample_db = catalog
        .databases
        .iter()
        .find(|d| d.name == "tableplus_rs_sample")
        .expect("sample database should exist");

    let customers = sample_db.schemas[0]
        .tables
        .iter()
        .find(|t| t.name == "customers")
        .expect("sample table `customers` should exist");
    assert_eq!(customers.primary_key, vec!["id".to_string()]);

    let orders = sample_db.schemas[0]
        .tables
        .iter()
        .find(|t| t.name == "orders")
        .expect("orders table should exist");
    assert_eq!(orders.foreign_keys.len(), 1);
    assert_eq!(orders.foreign_keys[0].referenced_table, "customers");
}

#[tokio::test]
async fn bulk_insert_then_select_reads_back_inserted_rows() {
    let (config, password) = test_config();
    let adapter = db_mysql::connect(&config, &password).await.expect("should connect");

    let columns = vec!["full_name".to_string(), "email".to_string(), "is_active".to_string()];
    let unique_email = format!("bulk-{}@example.com", uuid::Uuid::new_v4());
    let rows = vec![vec![
        Value::Text("Bulk Insert Test".to_string()),
        Value::Text(unique_email.clone()),
        Value::Bool(true),
    ]];

    let inserted = adapter
        .bulk_insert("customers", &columns, &rows)
        .await
        .expect("bulk insert should succeed");
    assert_eq!(inserted, 1);

    let result = adapter
        .execute(&format!("SELECT full_name FROM customers WHERE email = '{unique_email}'"))
        .await
        .expect("select should succeed");
    assert_eq!(result.rows.len(), 1);
    assert_eq!(result.rows[0][0], Value::Text("Bulk Insert Test".to_string()));
}
