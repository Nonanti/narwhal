//! DDL fetch test for the `MySQL` driver.
//!
//! Skipped unless the `NARWHAL_TEST_MYSQL_HOST` environment variable is set
//! (requires a running `MySQL` instance).

use narwhal_core::{ConnectionConfig, ConnectionParams, DatabaseDriver};
use narwhal_driver_mysql::MysqlDriver;
use uuid::Uuid;

fn mysql_config() -> Option<ConnectionConfig> {
    let host = std::env::var("NARWHAL_TEST_MYSQL_HOST").ok()?;
    let port: u16 = std::env::var("NARWHAL_TEST_MYSQL_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(3306);
    let db = std::env::var("NARWHAL_TEST_MYSQL_DB").ok()?;
    let user = std::env::var("NARWHAL_TEST_MYSQL_USER").ok()?;
    Some(ConnectionConfig {
        id: Uuid::nil(),
        name: "test".into(),
        driver: MysqlDriver::NAME.into(),
        params: ConnectionParams::with(|p| {
            p.host = Some(host);
            p.port = Some(port);
            p.database = Some(db);
            p.username = Some(user);
        }),
    })
}

#[tokio::test]
async fn fetch_ddl_returns_create_table() {
    let Some(config) = mysql_config() else {
        eprintln!("skipping — set NARWHAL_TEST_MYSQL_HOST/DB/USER to run");
        return;
    };

    let password = std::env::var("NARWHAL_TEST_MYSQL_PASSWORD").ok();
    let driver = MysqlDriver::new();
    let mut conn = driver
        .connect(&config, password.as_deref())
        .await
        .expect("connect to mysql");

    conn.execute("DROP TABLE IF EXISTS narwhal_ddl_users", &[])
        .await
        .ok();
    conn.execute(
        "CREATE TABLE narwhal_ddl_users (\
         id INT NOT NULL PRIMARY KEY, \
         name VARCHAR(255) NOT NULL, \
         email VARCHAR(255))",
        &[],
    )
    .await
    .expect("create table");

    let db = config.params.database.as_deref().unwrap_or("test");
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
