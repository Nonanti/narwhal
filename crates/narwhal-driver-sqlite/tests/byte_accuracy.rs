//! Byte-accurate row invariants for the SQLite driver.
//!
//! These tests verify that NULL, empty strings, invalid UTF-8, embedded
//! NUL bytes, tab/newline-in-string, and numeric edge values survive a
//! full round-trip through the driver without silent lossy conversion.

use narwhal_core::{Connection, ConnectionConfig, ConnectionParams, DatabaseDriver, Value};
use narwhal_driver_sqlite::SqliteDriver;

async fn test_connect() -> narwhal_core::Result<Box<dyn Connection>> {
    let dir = tempfile::tempdir().expect("tempdir for sqlite");
    let db_path = dir.path().join("byte_test.db");
    let path = db_path.to_str().expect("utf8 path");
    // Leak the TempDir so the file survives the connection lifetime.
    std::mem::forget(dir);

    let config = ConnectionConfig {
        id: uuid::Uuid::nil(),
        name: "byte_test".into(),
        driver: SqliteDriver::NAME.into(),
        params: ConnectionParams {
            path: Some(path.to_owned()),
            ..Default::default()
        },
    };
    SqliteDriver::new().connect(&config, None).await
}

#[tokio::test]
async fn null_vs_empty_string() -> narwhal_core::Result<()> {
    let mut conn = test_connect().await?;
    conn.execute("DROP TABLE IF EXISTS narwhal_byte_test", &[])
        .await?;
    conn.execute("CREATE TABLE narwhal_byte_test (a TEXT, b TEXT)", &[])
        .await?;
    conn.execute(
        "INSERT INTO narwhal_byte_test (a, b) VALUES (NULL, '')",
        &[],
    )
    .await?;
    let result = conn
        .execute("SELECT a, b FROM narwhal_byte_test", &[])
        .await?;
    assert_eq!(result.rows.len(), 1);
    assert!(matches!(result.rows[0].get(0), Some(Value::Null)));
    match result.rows[0].get(1) {
        Some(Value::String(s)) => assert_eq!(s, ""),
        other => panic!("expected Value::String(\"\"), got {other:?}"),
    }
    Ok(())
}

#[tokio::test]
async fn invalid_utf8_and_special_bytes_in_blob() -> narwhal_core::Result<()> {
    let mut conn = test_connect().await?;
    conn.execute("DROP TABLE IF EXISTS narwhal_byte_test", &[])
        .await?;
    conn.execute("CREATE TABLE narwhal_byte_test (data BLOB)", &[])
        .await?;

    // Invalid UTF-8 bytes.
    let bad_bytes = vec![0xFF, 0xFE, 0xFD];
    conn.execute(
        "INSERT INTO narwhal_byte_test (data) VALUES (?1)",
        &[Value::Bytes(bad_bytes.clone())],
    )
    .await?;
    let result = conn
        .execute("SELECT data FROM narwhal_byte_test", &[])
        .await?;
    assert_eq!(result.rows.len(), 1);
    match result.rows[0].get(0) {
        Some(Value::Bytes(b)) => assert_eq!(b, &bad_bytes),
        other => panic!("expected Value::Bytes, got {other:?}"),
    }

    // Embedded NUL byte in BLOB.
    conn.execute("DELETE FROM narwhal_byte_test", &[]).await?;
    let nul_blob = b"before\0after".to_vec();
    conn.execute(
        "INSERT INTO narwhal_byte_test (data) VALUES (?1)",
        &[Value::Bytes(nul_blob.clone())],
    )
    .await?;
    let result = conn
        .execute("SELECT data FROM narwhal_byte_test", &[])
        .await?;
    match result.rows[0].get(0) {
        Some(Value::Bytes(b)) => assert_eq!(b, &nul_blob),
        other => panic!("expected Value::Bytes with embedded NUL, got {other:?}"),
    }

    // Tab / newline in string.
    conn.execute("DELETE FROM narwhal_byte_test", &[]).await?;
    let tricky = "col\twith\nnewline";
    conn.execute(
        "INSERT INTO narwhal_byte_test (data) VALUES (?1)",
        &[Value::Bytes(tricky.as_bytes().to_vec())],
    )
    .await?;
    let result = conn
        .execute("SELECT data FROM narwhal_byte_test", &[])
        .await?;
    match result.rows[0].get(0) {
        Some(Value::Bytes(b)) => assert_eq!(b, tricky.as_bytes()),
        other => panic!("expected Value::Bytes with tab/newline, got {other:?}"),
    }

    Ok(())
}

#[tokio::test]
async fn numeric_edges() -> narwhal_core::Result<()> {
    let mut conn = test_connect().await?;
    conn.execute("DROP TABLE IF EXISTS narwhal_byte_test", &[])
        .await?;
    conn.execute("CREATE TABLE narwhal_byte_test (n INTEGER)", &[])
        .await?;

    // i64::MAX round-trip.
    conn.execute(
        "INSERT INTO narwhal_byte_test (n) VALUES (?1)",
        &[Value::Int(i64::MAX)],
    )
    .await?;
    let result = conn.execute("SELECT n FROM narwhal_byte_test", &[]).await?;
    match result.rows[0].get(0) {
        Some(Value::Int(n)) => assert_eq!(*n, i64::MAX),
        other => panic!("expected Value::Int(i64::MAX), got {other:?}"),
    }

    // i64::MIN round-trip.
    conn.execute("DELETE FROM narwhal_byte_test", &[]).await?;
    conn.execute(
        "INSERT INTO narwhal_byte_test (n) VALUES (?1)",
        &[Value::Int(i64::MIN)],
    )
    .await?;
    let result = conn.execute("SELECT n FROM narwhal_byte_test", &[]).await?;
    match result.rows[0].get(0) {
        Some(Value::Int(n)) => assert_eq!(*n, i64::MIN),
        other => panic!("expected Value::Int(i64::MIN), got {other:?}"),
    }

    // SQLite stores REAL as 8-byte IEEE 754, but NaN/Inf are not
    // representable — they are rejected or silently converted. Verify
    // that f64::INFINITY does not silently become a finite value.
    conn.execute("DELETE FROM narwhal_byte_test", &[]).await?;
    conn.execute("CREATE TABLE narwhal_float_test (f REAL)", &[])
        .await?;
    let insert_result = conn
        .execute(
            "INSERT INTO narwhal_float_test (f) VALUES (?1)",
            &[Value::Float(f64::INFINITY)],
        )
        .await;
    // Either the insert succeeds and the read value is still infinite,
    // or the insert/reject fails with an error — never silent loss.
    if insert_result.is_ok() {
        let result = conn
            .execute("SELECT f FROM narwhal_float_test", &[])
            .await?;
        match result.rows[0].get(0) {
            Some(Value::Float(f)) => assert!(f.is_infinite(), "expected Inf, got {f}"),
            other => panic!("expected Value::Float, got {other:?}"),
        }
    }

    Ok(())
}
