//! B4: verify that `SqliteDriver::connect` enables `PRAGMA foreign_keys = ON`
//! so that `REFERENCES … ON DELETE CASCADE` constraints are enforced at
//! runtime instead of being silently ignored.

#![cfg(feature = "sqlite")]

use narwhal_core::{ConnectionConfig, ConnectionParams, DatabaseDriver, Value};
use narwhal_drivers::sqlite::SqliteDriver;
use uuid::Uuid;

fn memory_config() -> ConnectionConfig {
    ConnectionConfig {
        id: Uuid::nil(),
        name: "test".into(),
        driver: SqliteDriver::NAME.into(),
        params: ConnectionParams::with(|p| {
            p.path = Some(":memory:".into());
        }),
    }
}

#[tokio::test]
async fn sqlite_foreign_keys_pragma_enabled() {
    let driver = SqliteDriver::new();
    let mut conn = driver
        .connect(&memory_config(), None)
        .await
        .expect("open in-memory database");

    conn.execute("CREATE TABLE p(id INTEGER PRIMARY KEY);", &[])
        .await
        .expect("create parent table");

    conn.execute(
        "CREATE TABLE c(id INTEGER PRIMARY KEY, p_id INTEGER REFERENCES p(id));",
        &[],
    )
    .await
    .expect("create child table");

    // PRAGMA foreign_keys must return 1 (ON).
    let result = conn
        .execute("PRAGMA foreign_keys;", &[])
        .await
        .expect("read pragma");
    let fk_flag = result
        .rows
        .first()
        .and_then(|r| r.get(0))
        .expect("row with pragma value");
    assert!(
        matches!(fk_flag, Value::Int(1)),
        "expected foreign_keys = 1, got {fk_flag:?}"
    );

    // An INSERT that violates the FK constraint must fail with a
    // foreign-key error. The rusqlite FK message sits in the error
    // source chain, so walk it the same way sqlite_read_only.rs does.
    let err = conn
        .execute("INSERT INTO c VALUES (1, 999);", &[])
        .await
        .expect_err("FK violation should fail");
    let mut combined = err.to_string().to_lowercase();
    let mut src: Option<&dyn std::error::Error> = std::error::Error::source(&err);
    while let Some(s) = src {
        combined.push_str(" | ");
        combined.push_str(&s.to_string().to_lowercase());
        src = s.source();
    }
    assert!(
        combined.contains("foreign key"),
        "expected 'foreign key' in error chain, got: {combined}"
    );
}
