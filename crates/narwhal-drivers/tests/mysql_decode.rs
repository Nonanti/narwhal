//! Regression tests for `value_from_my` (bug L29).
//!
//! Without column-type awareness, `MyValue::Bytes` was always decoded as
//! `Value::String` whenever the payload happened to be valid UTF-8. That
//! lost the BLOB/VARBINARY distinction: a JPEG that started with
//! `ff d8 ff` looked like garbage in the grid, while a small ASCII-only
//! BLOB silently turned into a string and broke equality checks against
//! `Value::Bytes` parameters bound by the caller.
//!
//! The fix routes the column type into `value_from_my`; binary column
//! types stay as `Value::Bytes`, everything else keeps the prior
//! "UTF-8 with fallback to bytes" behaviour.

use mysql_async::Value as MyValue;
use mysql_async::consts::ColumnType;
use narwhal_core::Value;
use narwhal_drivers::mysql::__test_only::value_from_my;

fn expect_bytes(v: Value, expected: &[u8]) {
    match v {
        Value::Bytes(b) => assert_eq!(b, expected),
        other => panic!("expected Value::Bytes({expected:?}), got {other:?}"),
    }
}

fn expect_string(v: Value, expected: &str) {
    match v {
        Value::String(s) => assert_eq!(s, expected),
        other => panic!("expected Value::String({expected:?}), got {other:?}"),
    }
}

fn expect_int(v: Value, expected: i64) {
    match v {
        Value::Int(n) => assert_eq!(n, expected),
        other => panic!("expected Value::Int({expected}), got {other:?}"),
    }
}

fn expect_null(v: Value) {
    match v {
        Value::Null => (),
        other => panic!("expected Value::Null, got {other:?}"),
    }
}

#[test]
fn blob_column_keeps_bytes_even_when_ascii() {
    // Valid UTF-8 payload from a BLOB column must NOT be decoded as a
    // string. The application layer expects raw bytes for binary types.
    let v = MyValue::Bytes(b"hello".to_vec());
    expect_bytes(value_from_my(&v, ColumnType::MYSQL_TYPE_BLOB), b"hello");
}

#[test]
fn tiny_blob_column_keeps_bytes() {
    let v = MyValue::Bytes(vec![0x00, 0x01, 0x02]);
    expect_bytes(
        value_from_my(&v, ColumnType::MYSQL_TYPE_TINY_BLOB),
        &[0x00, 0x01, 0x02],
    );
}

#[test]
fn medium_blob_column_keeps_bytes() {
    let v = MyValue::Bytes(b"data".to_vec());
    expect_bytes(
        value_from_my(&v, ColumnType::MYSQL_TYPE_MEDIUM_BLOB),
        b"data",
    );
}

#[test]
fn long_blob_column_keeps_bytes() {
    let v = MyValue::Bytes(b"data".to_vec());
    expect_bytes(value_from_my(&v, ColumnType::MYSQL_TYPE_LONG_BLOB), b"data");
}

#[test]
fn varchar_column_decodes_utf8_string() {
    let v = MyValue::Bytes("naïveté".as_bytes().to_vec());
    expect_string(value_from_my(&v, ColumnType::MYSQL_TYPE_VARCHAR), "naïveté");
}

#[test]
fn varchar_with_invalid_utf8_falls_back_to_bytes() {
    let v = MyValue::Bytes(vec![0xff, 0xfe, 0x00]);
    expect_bytes(
        value_from_my(&v, ColumnType::MYSQL_TYPE_VARCHAR),
        &[0xff, 0xfe, 0x00],
    );
}

#[test]
fn var_string_column_decodes_utf8_string() {
    let v = MyValue::Bytes(b"plain".to_vec());
    expect_string(
        value_from_my(&v, ColumnType::MYSQL_TYPE_VAR_STRING),
        "plain",
    );
}

#[test]
fn null_value_decodes_as_null_regardless_of_column_type() {
    expect_null(value_from_my(&MyValue::NULL, ColumnType::MYSQL_TYPE_BLOB));
    expect_null(value_from_my(
        &MyValue::NULL,
        ColumnType::MYSQL_TYPE_VARCHAR,
    ));
}

#[test]
fn int_value_unaffected_by_column_type() {
    // `value_from_my` only branches on column type for Bytes payloads;
    // other variants pass through untouched.
    let v = MyValue::Int(42);
    expect_int(value_from_my(&v, ColumnType::MYSQL_TYPE_LONG), 42);
    expect_int(value_from_my(&v, ColumnType::MYSQL_TYPE_BLOB), 42);
}
