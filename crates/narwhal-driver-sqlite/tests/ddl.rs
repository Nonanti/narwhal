//! DDL fetch test for the SQLite driver.

use narwhal_core::{ConnectionConfig, ConnectionParams, DatabaseDriver};
use narwhal_driver_sqlite::SqliteDriver;
use uuid::Uuid;

fn memory_config() -> ConnectionConfig {
    ConnectionConfig {
        id: Uuid::nil(),
        name: "test".into(),
        driver: SqliteDriver::NAME.into(),
        params: ConnectionParams {
            path: Some(":memory:".into()),
            ..Default::default()
        },
    }
}

#[tokio::test]
async fn fetch_ddl_returns_create_table() {
    let driver = SqliteDriver::new();
    let mut conn = driver
        .connect(&memory_config(), None)
        .await
        .expect("open in-memory database");

    conn.execute(
        "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT NOT NULL, email TEXT UNIQUE)",
        &[],
    )
    .await
    .expect("create table");

    let ddl = conn.fetch_ddl("main", "users").await.expect("fetch_ddl");

    assert!(ddl.contains("users"), "DDL must contain table name: {ddl}");
    assert!(ddl.contains("id"), "DDL must contain column 'id': {ddl}");
    assert!(
        ddl.contains("name"),
        "DDL must contain column 'name': {ddl}"
    );
    assert!(
        ddl.contains("email"),
        "DDL must contain column 'email': {ddl}"
    );
}

#[tokio::test]
async fn fetch_ddl_nonexistent_table_returns_error() {
    let driver = SqliteDriver::new();
    let mut conn = driver
        .connect(&memory_config(), None)
        .await
        .expect("open in-memory database");

    let result = conn.fetch_ddl("main", "nonexistent").await;
    assert!(result.is_err(), "should fail for nonexistent table");
}
