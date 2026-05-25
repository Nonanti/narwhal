//! Issue C (sprint 5): `DuckDB` does not expose a session-level
//! read-only switch, but the trait method must still surface that fact
//! through a typed `Error::Unsupported` so the MCP layer can warn the
//! operator and fall back to the statement guard.

use narwhal_core::{ConnectionConfig, ConnectionParams, DatabaseDriver, Error};
use narwhal_driver_duckdb::DuckdbDriver;
use uuid::Uuid;

fn memory_config() -> ConnectionConfig {
    ConnectionConfig {
        id: Uuid::nil(),
        name: "test".into(),
        driver: DuckdbDriver::NAME.into(),
        params: ConnectionParams {
            path: Some(":memory:".into()),
            ..Default::default()
        },
    }
}

#[tokio::test]
async fn set_read_only_true_is_unsupported_with_hint() {
    let driver = DuckdbDriver::new();
    let mut conn = driver
        .connect(&memory_config(), None)
        .await
        .expect("open in-memory database");

    let err = conn
        .set_read_only(true)
        .await
        .expect_err("DuckDB lacks a session-level read-only switch");

    match err {
        Error::Unsupported(msg) => {
            assert!(
                msg.to_lowercase().contains("access_mode"),
                "hint must mention `access_mode` (got: {msg})"
            );
        }
        other => panic!("expected Error::Unsupported, got {other:?}"),
    }
}

#[tokio::test]
async fn set_read_only_false_is_a_noop() {
    let driver = DuckdbDriver::new();
    let mut conn = driver
        .connect(&memory_config(), None)
        .await
        .expect("open in-memory database");

    // Turning enforcement OFF when it was never ON must succeed
    // silently so callers can flip the flag back without special-casing
    // the engine.
    conn.set_read_only(false)
        .await
        .expect("set_read_only(false) must be a no-op on DuckDB");
}
