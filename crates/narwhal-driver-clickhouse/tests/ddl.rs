//! DDL fetch test for the ClickHouse driver.
//!
//! Skipped unless the `NARWHAL_TEST_CH_HOST` environment variable is set
//! (requires a running ClickHouse instance).

use narwhal_core::{ConnectionConfig, ConnectionParams, DatabaseDriver};
use narwhal_driver_clickhouse::ClickhouseDriver;
use uuid::Uuid;

fn ch_config() -> Option<ConnectionConfig> {
    let host = std::env::var("NARWHAL_TEST_CH_HOST").ok()?;
    let port: u16 = std::env::var("NARWHAL_TEST_CH_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(8123);
    let db = std::env::var("NARWHAL_TEST_CH_DB")
        .ok()
        .unwrap_or_else(|| "default".into());
    let user = std::env::var("NARWHAL_TEST_CH_USER")
        .ok()
        .unwrap_or_else(|| "default".into());
    Some(ConnectionConfig {
        id: Uuid::nil(),
        name: "test".into(),
        driver: ClickhouseDriver::NAME.into(),
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
    let Some(config) = ch_config() else {
        eprintln!("skipping — set NARWHAL_TEST_CH_HOST to run");
        return;
    };

    let password = std::env::var("NARWHAL_TEST_CH_PASSWORD").ok();
    let driver = ClickhouseDriver::new();
    let mut conn = driver
        .connect(&config, password.as_deref())
        .await
        .expect("connect to clickhouse");

    let db = config.params.database.as_deref().unwrap_or("default");

    conn.execute("DROP TABLE IF EXISTS narwhal_ddl_users", &[])
        .await
        .ok();
    conn.execute(
        "CREATE TABLE narwhal_ddl_users (\
         id UInt32, \
         name String, \
         email Nullable(String))\
         ENGINE = MergeTree()\
         ORDER BY id",
        &[],
    )
    .await
    .expect("create table");

    let ddl = conn
        .fetch_ddl(db, "narwhal_ddl_users")
        .await
        .expect("fetch_ddl");

    assert!(
        ddl.contains("narwhal_ddl_users"),
        "DDL must contain table name: {ddl}"
    );
    assert!(ddl.contains("id"), "DDL must contain column 'id': {ddl}");
    assert!(
        ddl.contains("name"),
        "DDL must contain column 'name': {ddl}"
    );

    // Cleanup.
    conn.execute("DROP TABLE IF EXISTS narwhal_ddl_users", &[])
        .await
        .ok();
}
