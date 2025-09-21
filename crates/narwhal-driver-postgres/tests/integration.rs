//! End-to-end integration tests against an ephemeral PostgreSQL container.
//!
//! These tests require Docker to be running and are therefore marked
//! `#[ignore]`. Run them locally with:
//!
//! ```sh
//! cargo test -p narwhal-driver-postgres -- --ignored
//! ```

use narwhal_core::{
    Connection, ConnectionConfig, ConnectionParams, DatabaseDriver, IsolationLevel, Value,
};
use narwhal_driver_postgres::PostgresDriver;
use testcontainers::runners::AsyncRunner;
use testcontainers_modules::postgres::Postgres;
use uuid::Uuid;

struct Harness {
    _container: testcontainers::ContainerAsync<Postgres>,
    driver: PostgresDriver,
    config: ConnectionConfig,
    password: String,
}

impl Harness {
    async fn start() -> Self {
        let container = Postgres::default()
            .start()
            .await
            .expect("start postgres container");
        let port = container
            .get_host_port_ipv4(5432)
            .await
            .expect("postgres host port");

        let config = ConnectionConfig {
            id: Uuid::nil(),
            name: "it".into(),
            driver: PostgresDriver::NAME.into(),
            params: ConnectionParams {
                host: Some("127.0.0.1".into()),
                port: Some(port),
                database: Some("postgres".into()),
                username: Some("postgres".into()),
                ..Default::default()
            },
        };

        Self {
            _container: container,
            driver: PostgresDriver::new(),
            config,
            password: "postgres".into(),
        }
    }

    async fn connect(&self) -> Box<dyn Connection> {
        self.driver
            .connect(&self.config, Some(&self.password))
            .await
            .expect("driver connect")
    }
}

#[tokio::test]
#[ignore = "requires docker"]
async fn round_trip_select_and_parameter_binding() {
    let h = Harness::start().await;
    let mut conn = h.connect().await;

    conn.execute(
        "CREATE TABLE items (id SERIAL PRIMARY KEY, name TEXT NOT NULL, qty INT)",
        &[],
    )
    .await
    .unwrap();

    let insert = conn
        .execute(
            "INSERT INTO items (name, qty) VALUES ($1, $2)",
            &[Value::String("widget".into()), Value::Int(7)],
        )
        .await
        .unwrap();
    assert_eq!(insert.rows_affected, Some(1));

    let select = conn
        .execute(
            "SELECT name, qty FROM items WHERE qty >= $1",
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
async fn streaming_consumes_rows_lazily() {
    let h = Harness::start().await;
    let mut conn = h.connect().await;

    conn.execute("CREATE TABLE nums (n INT)", &[])
        .await
        .unwrap();
    conn.execute("INSERT INTO nums SELECT generate_series(1, 100)", &[])
        .await
        .unwrap();

    let mut stream = conn
        .stream("SELECT n FROM nums ORDER BY n", &[])
        .await
        .unwrap();
    assert_eq!(stream.columns().len(), 1);

    let mut total: i64 = 0;
    let mut count: i64 = 0;
    while let Some(row) = stream.next_row().await.unwrap() {
        if let Some(Value::Int(n)) = row.get(0) {
            total += *n;
            count += 1;
        }
    }
    assert_eq!(count, 100);
    assert_eq!(total, (1..=100).sum::<i64>());
}

#[tokio::test]
#[ignore = "requires docker"]
async fn transaction_rollback_discards_changes() {
    let h = Harness::start().await;
    let mut conn = h.connect().await;

    conn.execute("CREATE TABLE counters (k TEXT PRIMARY KEY, v INT)", &[])
        .await
        .unwrap();

    conn.begin_with(IsolationLevel::Serializable).await.unwrap();
    conn.execute(
        "INSERT INTO counters VALUES ($1, $2)",
        &[Value::String("a".into()), Value::Int(1)],
    )
    .await
    .unwrap();
    conn.rollback().await.unwrap();

    let select = conn
        .execute("SELECT count(*) FROM counters", &[])
        .await
        .unwrap();
    assert_eq!(select.rows[0].get(0).map(Value::render), Some("0".into()));
}

#[tokio::test]
#[ignore = "requires docker"]
async fn schema_introspection() {
    let h = Harness::start().await;
    let mut conn = h.connect().await;

    conn.execute(
        "CREATE TABLE products (
            id SERIAL PRIMARY KEY,
            sku TEXT NOT NULL UNIQUE,
            price NUMERIC(10, 2) DEFAULT 0
        )",
        &[],
    )
    .await
    .unwrap();

    let schemas = conn.list_schemas().await.unwrap();
    assert!(schemas.iter().any(|s| s.name == "public"));

    let tables = conn.list_tables("public").await.unwrap();
    assert!(tables.iter().any(|t| t.name == "products"));

    let schema = conn.describe_table("public", "products").await.unwrap();
    assert_eq!(schema.columns.len(), 3);
    let id = schema
        .columns
        .iter()
        .find(|c| c.name == "id")
        .expect("id column");
    assert!(id.primary_key);
}
