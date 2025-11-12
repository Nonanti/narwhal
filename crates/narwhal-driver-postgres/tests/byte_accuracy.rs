//! Byte-accurate row invariants for the PostgreSQL driver.
//!
//! These tests verify that NULL, empty strings, invalid UTF-8, embedded
//! NUL bytes, tab/newline-in-string, and numeric edge values survive a
//! full round-trip through the driver without silent lossy conversion.
//!
//! Tests are skipped gracefully when `NARWHAL_POSTGRES_URL` is not set.

mod common;

use common::test_connect;
use narwhal_core::Value;

#[tokio::test]
async fn null_vs_empty_string() -> narwhal_core::Result<()> {
    let Some(mut conn) = test_connect().await? else {
        return Ok(());
    };
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
async fn invalid_utf8_and_special_bytes_in_bytea() -> narwhal_core::Result<()> {
    let Some(mut conn) = test_connect().await? else {
        return Ok(());
    };
    conn.execute("DROP TABLE IF EXISTS narwhal_byte_test", &[])
        .await?;
    conn.execute("CREATE TABLE narwhal_byte_test (data BYTEA)", &[])
        .await?;

    // Invalid UTF-8 bytes in BYTEA column.
    let bad_bytes = vec![0xFF, 0xFE, 0xFD];
    conn.execute(
        "INSERT INTO narwhal_byte_test (data) VALUES ($1)",
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

    // Embedded NUL byte in BYTEA.
    conn.execute("DELETE FROM narwhal_byte_test", &[]).await?;
    let nul_blob = b"before\0after".to_vec();
    conn.execute(
        "INSERT INTO narwhal_byte_test (data) VALUES ($1)",
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

    // Tab / newline in string (TEXT column).
    conn.execute("DROP TABLE IF EXISTS narwhal_text_test", &[])
        .await?;
    conn.execute("CREATE TABLE narwhal_text_test (s TEXT)", &[])
        .await?;
    let tricky = "col\twith\nnewline";
    conn.execute(
        "INSERT INTO narwhal_text_test (s) VALUES ($1)",
        &[Value::String(tricky.to_owned())],
    )
    .await?;
    let result = conn.execute("SELECT s FROM narwhal_text_test", &[]).await?;
    match result.rows[0].get(0) {
        Some(Value::String(s)) => assert_eq!(s, tricky),
        other => panic!("expected Value::String with tab/newline, got {other:?}"),
    }

    Ok(())
}

#[tokio::test]
async fn numeric_edges() -> narwhal_core::Result<()> {
    let Some(mut conn) = test_connect().await? else {
        return Ok(());
    };
    conn.execute("DROP TABLE IF EXISTS narwhal_byte_test", &[])
        .await?;
    conn.execute("CREATE TABLE narwhal_byte_test (n BIGINT)", &[])
        .await?;

    // i64::MAX round-trip.
    conn.execute(
        "INSERT INTO narwhal_byte_test (n) VALUES ($1)",
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
        "INSERT INTO narwhal_byte_test (n) VALUES ($1)",
        &[Value::Int(i64::MIN)],
    )
    .await?;
    let result = conn.execute("SELECT n FROM narwhal_byte_test", &[]).await?;
    match result.rows[0].get(0) {
        Some(Value::Int(n)) => assert_eq!(*n, i64::MIN),
        other => panic!("expected Value::Int(i64::MIN), got {other:?}"),
    }

    // PostgreSQL supports IEEE 754 Inf and NaN for double precision.
    // Verify they round-trip as Value::Float without silent loss.
    conn.execute("DROP TABLE IF EXISTS narwhal_float_test", &[])
        .await?;
    conn.execute("CREATE TABLE narwhal_float_test (f DOUBLE PRECISION)", &[])
        .await?;

    // Positive infinity.
    conn.execute(
        "INSERT INTO narwhal_float_test (f) VALUES ('infinity')",
        &[],
    )
    .await?;
    let result = conn
        .execute("SELECT f FROM narwhal_float_test", &[])
        .await?;
    match result.rows[0].get(0) {
        Some(Value::Float(f)) => assert!(
            f.is_infinite() && f.is_sign_positive(),
            "expected +Inf, got {f}"
        ),
        other => panic!("expected Value::Float, got {other:?}"),
    }

    // NaN.
    conn.execute("DELETE FROM narwhal_float_test", &[]).await?;
    conn.execute("INSERT INTO narwhal_float_test (f) VALUES ('nan')", &[])
        .await?;
    let result = conn
        .execute("SELECT f FROM narwhal_float_test", &[])
        .await?;
    match result.rows[0].get(0) {
        Some(Value::Float(f)) => assert!(f.is_nan(), "expected NaN, got {f}"),
        other => panic!("expected Value::Float, got {other:?}"),
    }

    Ok(())
}
