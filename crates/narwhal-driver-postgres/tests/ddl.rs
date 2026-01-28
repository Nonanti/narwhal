//! DDL fetch test for the `PostgreSQL` driver.
//!
//! Skipped unless the `NARWHAL_TEST_PG_HOST` environment variable is set
//! (requires a running `PostgreSQL` instance).

use narwhal_core::{ConnectionConfig, ConnectionParams, DatabaseDriver};
use narwhal_driver_postgres::PostgresDriver;
use uuid::Uuid;

fn pg_config() -> Option<ConnectionConfig> {
    let host = std::env::var("NARWHAL_TEST_PG_HOST").ok()?;
    let port: u16 = std::env::var("NARWHAL_TEST_PG_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(5432);
    let db = std::env::var("NARWHAL_TEST_PG_DB").ok()?;
    let user = std::env::var("NARWHAL_TEST_PG_USER").ok()?;
    Some(ConnectionConfig {
        id: Uuid::nil(),
        name: "test".into(),
        driver: PostgresDriver::NAME.into(),
        params: ConnectionParams {
            host: Some(host),
            port: Some(port),
            database: Some(db),
            username: Some(user),
            ..Default::default()
        },
    })
}

#[tokio::test]
async fn fetch_ddl_returns_create_table() {
    let Some(config) = pg_config() else {
        eprintln!("skipping — set NARWHAL_TEST_PG_HOST/DB/USER to run");
        return;
    };

    let password = std::env::var("NARWHAL_TEST_PG_PASSWORD").ok();
    let driver = PostgresDriver::new();
    let mut conn = driver
        .connect(&config, password.as_deref())
        .await
        .expect("connect to postgres");

    // Create a test table in a temporary schema to avoid conflicts.
    conn.execute("DROP SCHEMA IF EXISTS narwhal_ddl_test CASCADE", &[])
        .await
        .ok();
    conn.execute("CREATE SCHEMA narwhal_ddl_test", &[])
        .await
        .expect("create schema");
    conn.execute(
        "CREATE TABLE narwhal_ddl_test.users (\
         id INTEGER NOT NULL PRIMARY KEY, \
         name TEXT NOT NULL, \
         email TEXT, \
         default_email TEXT DEFAULT 'none@example.com')",
        &[],
    )
    .await
    .expect("create table");

    let ddl = conn
        .fetch_ddl("narwhal_ddl_test", "users")
        .await
        .expect("fetch_ddl");

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
    // NOT NULL must survive reconstruction.
    assert!(ddl.contains("NOT NULL"), "DDL must contain NOT NULL: {ddl}");
    // PRIMARY KEY must survive.
    assert!(
        ddl.contains("PRIMARY KEY"),
        "DDL must contain PRIMARY KEY: {ddl}"
    );

    // Cleanup.
    conn.execute("DROP SCHEMA narwhal_ddl_test CASCADE", &[])
        .await
        .ok();
}

#[tokio::test]
async fn ddl_generated_identity_column() {
    let Some(config) = pg_config() else {
        eprintln!("skipping — set NARWHAL_TEST_PG_HOST/DB/USER to run");
        return;
    };

    let password = std::env::var("NARWHAL_TEST_PG_PASSWORD").ok();
    let driver = PostgresDriver::new();
    let mut conn = driver
        .connect(&config, password.as_deref())
        .await
        .expect("connect to postgres");

    conn.execute("DROP SCHEMA IF EXISTS narwhal_ddl_test CASCADE", &[])
        .await
        .ok();
    conn.execute("CREATE SCHEMA narwhal_ddl_test", &[])
        .await
        .expect("create schema");
    conn.execute(
        "CREATE TABLE narwhal_ddl_test.orders (id INTEGER GENERATED ALWAYS AS IDENTITY PRIMARY KEY, label TEXT)",
        &[],
    )
    .await
    .expect("create table with identity");

    let ddl = conn
        .fetch_ddl("narwhal_ddl_test", "orders")
        .await
        .expect("fetch_ddl");

    assert!(
        ddl.contains("GENERATED ALWAYS AS IDENTITY"),
        "DDL must preserve GENERATED ALWAYS AS IDENTITY: {ddl}"
    );
    // Must NOT contain nextval (that is the wrong reconstruction).
    assert!(
        !ddl.contains("nextval"),
        "DDL must not expose raw nextval for identity column: {ddl}"
    );

    conn.execute("DROP SCHEMA narwhal_ddl_test CASCADE", &[])
        .await
        .ok();
}

#[tokio::test]
async fn ddl_generated_stored_column() {
    let Some(config) = pg_config() else {
        eprintln!("skipping — set NARWHAL_TEST_PG_HOST/DB/USER to run");
        return;
    };

    let password = std::env::var("NARWHAL_TEST_PG_PASSWORD").ok();
    let driver = PostgresDriver::new();
    let mut conn = driver
        .connect(&config, password.as_deref())
        .await
        .expect("connect to postgres");

    conn.execute("DROP SCHEMA IF EXISTS narwhal_ddl_test CASCADE", &[])
        .await
        .ok();
    conn.execute("CREATE SCHEMA narwhal_ddl_test", &[])
        .await
        .expect("create schema");
    conn.execute(
        "CREATE TABLE narwhal_ddl_test.products (\
         price numeric NOT NULL, \
         quantity integer NOT NULL, \
         total numeric GENERATED ALWAYS AS (price * quantity) STORED)",
        &[],
    )
    .await
    .expect("create table with stored generated");

    let ddl = conn
        .fetch_ddl("narwhal_ddl_test", "products")
        .await
        .expect("fetch_ddl");

    assert!(
        ddl.contains("GENERATED ALWAYS AS") && ddl.contains("STORED"),
        "DDL must preserve GENERATED ALWAYS AS (...) STORED: {ddl}"
    );

    conn.execute("DROP SCHEMA narwhal_ddl_test CASCADE", &[])
        .await
        .ok();
}

#[tokio::test]
async fn ddl_default_now() {
    let Some(config) = pg_config() else {
        eprintln!("skipping — set NARWHAL_TEST_PG_HOST/DB/USER to run");
        return;
    };

    let password = std::env::var("NARWHAL_TEST_PG_PASSWORD").ok();
    let driver = PostgresDriver::new();
    let mut conn = driver
        .connect(&config, password.as_deref())
        .await
        .expect("connect to postgres");

    conn.execute("DROP SCHEMA IF EXISTS narwhal_ddl_test CASCADE", &[])
        .await
        .ok();
    conn.execute("CREATE SCHEMA narwhal_ddl_test", &[])
        .await
        .expect("create schema");
    conn.execute(
        "CREATE TABLE narwhal_ddl_test.events (\
         id SERIAL PRIMARY KEY, \
         created_at TIMESTAMPTZ NOT NULL DEFAULT now())",
        &[],
    )
    .await
    .expect("create table with default now()");

    let ddl = conn
        .fetch_ddl("narwhal_ddl_test", "events")
        .await
        .expect("fetch_ddl");

    assert!(
        ddl.contains("now()"),
        "DDL must preserve DEFAULT now(): {ddl}"
    );

    conn.execute("DROP SCHEMA narwhal_ddl_test CASCADE", &[])
        .await
        .ok();
}
