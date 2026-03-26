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
    // C3: walk the source chain — the rusqlite error is now in `Error::source`,
    // not flattened into the top-level Display string.
    let mut combined = err.to_string();
    let mut src: Option<&dyn std::error::Error> = std::error::Error::source(&err);
    while let Some(s) = src {
        combined.push_str(" | ");
        combined.push_str(&s.to_string());
        src = s.source();
    }
    assert!(
        combined.contains("read")
            || combined.contains("query_only")
            || combined.contains("attempt to write"),
        "got: {combined}"
    );
    conn.set_read_only(false).await.unwrap();
    conn.execute("INSERT INTO t VALUES (2)", &[]).await.unwrap();
    let result = conn.execute("SELECT * FROM t", &[]).await.unwrap();
    // Only the second INSERT (after re-enabling writes) landed.
    assert_eq!(result.rows.len(), 1);
}
