//! Integration tests for the `ClickHouse` driver.
//!
//! These tests require a running `ClickHouse` server and are marked
//! `#[ignore]` so they don't run in CI. To execute them locally with
//! Docker:
//!
//! ```sh
//! docker run -d --name ch -p 8123:8123 -p 9000:9000 clickhouse/clickhouse-server:latest
//! cargo test -p narwhal-driver-clickhouse --test integration -- --ignored
//! docker stop ch && docker rm ch
//! ```

use narwhal_core::{ConnectionConfig, ConnectionParams, DatabaseDriver, Value};
use narwhal_drivers::clickhouse::ClickhouseDriver;
use uuid::Uuid;

fn config() -> ConnectionConfig {
    ConnectionConfig {
        id: Uuid::nil(),
        name: "test".into(),
        driver: ClickhouseDriver::NAME.into(),
        params: ConnectionParams::with(|p| {
            p.host = Some("localhost".into());
            p.port = Some(8123);
            p.username = Some("default".into());
            p.database = Some("default".into());
        }),
    }
}

async fn open() -> Box<dyn narwhal_core::DynConnection> {
    ClickhouseDriver::new()
        .connect(&config(), None)
        .await
        .expect("connect to ClickHouse")
}

#[tokio::test]
#[ignore]
async fn ping() {
    let mut conn = open().await;
    conn.ping().await.expect("ping should succeed");
}

#[tokio::test]
#[ignore]
async fn select_scalar() {
    let mut conn = open().await;
    let result = conn
        .execute("SELECT 1 AS one, 'hello' AS greeting", &[])
        .await
        .expect("select");
    assert_eq!(result.columns.len(), 2);
    assert_eq!(result.rows.len(), 1);
    assert_eq!(result.rows[0].get(0).map(Value::render), Some("1".into()));
    assert_eq!(
        result.rows[0].get(1).map(Value::render),
        Some("hello".into())
    );
}

#[tokio::test]
#[ignore]
async fn create_insert_select() {
    let mut conn = open().await;
    conn.execute(
        "CREATE TABLE IF NOT EXISTS test_tbl (id UInt32, name String) ENGINE = MergeTree() ORDER BY id",
        &[],
    )
    .await
    .expect("create table");
    conn.execute("INSERT INTO test_tbl VALUES (1, 'alice')", &[])
        .await
        .expect("insert");
    let result = conn
        .execute("SELECT id, name FROM test_tbl ORDER BY id", &[])
        .await
        .expect("select");
    assert_eq!(result.rows.len(), 1);
    assert_eq!(result.rows[0].get(0).map(Value::render), Some("1".into()));
    assert_eq!(
        result.rows[0].get(1).map(Value::render),
        Some("alice".into())
    );
    conn.execute("DROP TABLE IF EXISTS test_tbl", &[])
        .await
        .expect("drop table");
}

#[tokio::test]
#[ignore]
async fn list_schemas() {
    let mut conn = open().await;
    let schemas = conn.list_schemas().await.expect("list schemas");
    assert!(!schemas.is_empty());
    assert!(schemas.iter().any(|s| s.name == "default"));
}

#[tokio::test]
#[ignore]
async fn stream_yields_rows() {
    let mut conn = open().await;
    let mut stream = conn
        .stream("SELECT number FROM numbers(5) ORDER BY number", &[])
        .await
        .expect("stream");
    let mut collected = Vec::new();
    while let Some(row) = stream.next_row().await.expect("next_row") {
        collected.push(row.get(0).map(Value::render).unwrap_or_default());
    }
    assert_eq!(collected, vec!["0", "1", "2", "3", "4"]);
}
