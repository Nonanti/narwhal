use narwhal_core::{ConnectionConfig, ConnectionParams, DatabaseDriver};
use narwhal_drivers::sqlite::SqliteDriver;
use uuid::Uuid;

#[tokio::test]
async fn sqlite_set_read_only_blocks_writes() {
    let driver = SqliteDriver::new();
    let config = ConnectionConfig {
        id: Uuid::nil(),
        name: "test".into(),
        driver: SqliteDriver::NAME.into(),
        params: ConnectionParams::with(|p| {
            p.path = Some(":memory:".into());
        }),
    };
    let mut conn = driver.connect(&config, None).await.unwrap();
    conn.execute("CREATE TABLE t (x INT)", &[]).await.unwrap();
    conn.set_read_only(true).await.unwrap();
    let err = conn
        .execute("INSERT INTO t VALUES (1)", &[])
        .await
        .unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("read") || msg.contains("query_only") || msg.contains("attempt to write"),
        "got: {msg}"
    );
    conn.set_read_only(false).await.unwrap();
    conn.execute("INSERT INTO t VALUES (2)", &[]).await.unwrap();
    let result = conn.execute("SELECT * FROM t", &[]).await.unwrap();
    // Only the second INSERT (after re-enabling writes) landed.
    assert_eq!(result.rows.len(), 1);
}
