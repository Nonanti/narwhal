//! End-to-end integration tests against an ephemeral `MySQL` container.
//!
//! These tests require Docker. Run with:
//!
//! ```sh
//! cargo test -p narwhal-driver-mysql -- --ignored
//! ```

use std::time::Duration;

use narwhal_core::{ConnectionConfig, ConnectionParams, DatabaseDriver, IsolationLevel, Value};
use narwhal_drivers::mysql::MysqlDriver;
use testcontainers::runners::AsyncRunner;
use testcontainers_modules::mysql::Mysql;
use uuid::Uuid;

struct Harness {
    _container: testcontainers::ContainerAsync<Mysql>,
    driver: MysqlDriver,
    config: ConnectionConfig,
}

impl Harness {
    async fn start() -> Self {
        let container = Mysql::default()
            .start()
            .await
            .expect("start mysql container");
        let port = container
            .get_host_port_ipv4(3306)
            .await
            .expect("mysql host port");

        // testcontainers-modules' Mysql image bootstraps a database called
        // `test` with the user `root` and no password by default.
        let config = ConnectionConfig {
            id: Uuid::nil(),
            name: "it".into(),
            driver: MysqlDriver::NAME.into(),
            params: ConnectionParams::with(|p| {
                p.host = Some("127.0.0.1".into());
                p.port = Some(port);
                p.database = Some("test".into());
                p.username = Some("root".into());
            }),
        };

        // Wait briefly for the server to accept TCP connections.
        tokio::time::sleep(Duration::from_secs(1)).await;

        Self {
            _container: container,
            driver: MysqlDriver::new(),
            config,
        }
    }

    async fn connect(&self) -> Box<dyn narwhal_core::DynConnection> {
        for _ in 0..20 {
            if let Ok(conn) = self.driver.connect(&self.config, None).await {
                return conn;
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        panic!("mysql refused connections after 10 seconds")
    }
}

#[tokio::test]
#[ignore = "requires docker"]
async fn round_trip_select_and_parameter_binding() {
    let h = Harness::start().await;
    let mut conn = h.connect().await;

    conn.execute(
        "CREATE TABLE items (
            id INT PRIMARY KEY AUTO_INCREMENT,
            name VARCHAR(64) NOT NULL,
            qty INT
        )",
        &[],
    )
    .await
    .unwrap();

    let insert = conn
        .execute(
            "INSERT INTO items (name, qty) VALUES (?, ?)",
            &[Value::String("widget".into()), Value::Int(7)],
        )
        .await
        .unwrap();
    assert_eq!(insert.rows_affected, Some(1));

    let select = conn
        .execute(
            "SELECT name, qty FROM items WHERE qty >= ?",
            &[Value::Int(1)],
        )
        .await
        .unwrap();
    assert_eq!(select.rows.len(), 1);
    assert_eq!(
        select.rows[0].get(0).map(Value::render),
        Some("widget".into())
    );
    assert_eq!(select.rows[0].get(1).map(Value::render), Some("7".into()));
}

#[tokio::test]
#[ignore = "requires docker"]
async fn savepoint_partial_rollback() {
    let h = Harness::start().await;
    let mut conn = h.connect().await;

    conn.execute("CREATE TABLE t (n INT) ENGINE=InnoDB", &[])
        .await
        .unwrap();
    conn.begin().await.unwrap();
    conn.execute("INSERT INTO t VALUES (1)", &[]).await.unwrap();
    conn.savepoint("sp1").await.unwrap();
    conn.execute("INSERT INTO t VALUES (2)", &[]).await.unwrap();
    conn.rollback_to_savepoint("sp1").await.unwrap();
    conn.release_savepoint("sp1").await.unwrap();
    conn.commit().await.unwrap();

    let result = conn
        .execute("SELECT n FROM t ORDER BY n", &[])
        .await
        .unwrap();
    assert_eq!(result.rows.len(), 1);
    assert_eq!(result.rows[0].get(0).map(Value::render), Some("1".into()));
}

#[tokio::test]
#[ignore = "requires docker"]
async fn transaction_isolation_levels_apply() {
    let h = Harness::start().await;
    let mut conn = h.connect().await;

    conn.begin_with(IsolationLevel::Serializable).await.unwrap();
    conn.rollback().await.unwrap();

    conn.begin_with(IsolationLevel::ReadCommitted)
        .await
        .unwrap();
    conn.rollback().await.unwrap();
}

#[tokio::test]
#[ignore = "requires docker"]
async fn schema_introspection() {
    let h = Harness::start().await;
    let mut conn = h.connect().await;

    conn.execute(
        "CREATE TABLE products (
            id INT PRIMARY KEY AUTO_INCREMENT,
            sku VARCHAR(32) NOT NULL UNIQUE,
            price DECIMAL(10, 2) DEFAULT 0
        )",
        &[],
    )
    .await
    .unwrap();

    let schemas = conn.list_schemas().await.unwrap();
    assert!(schemas.iter().any(|s| s.name == "test"));

    let tables = conn.list_tables("test").await.unwrap();
    assert!(tables.iter().any(|t| t.name == "products"));

    let schema = conn.describe_table("test", "products").await.unwrap();
    assert_eq!(schema.columns.len(), 3);
    let id = schema
        .columns
        .iter()
        .find(|c| c.name == "id")
        .expect("id column");
    assert!(id.primary_key);
}
