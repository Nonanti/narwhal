//! Conversion layer between [`narwhal_core::Value`] and `rusqlite`.

use narwhal_core::Value;
use rusqlite::types::{Value as SqlValue, ValueRef};

/// Convert a [`Value`] into a `rusqlite` owned value suitable for parameter
/// binding. Types that have no native SQLite representation are encoded as
/// their canonical textual form (timestamps as RFC 3339, UUIDs as hyphenated
/// hex, JSON as its serialised form).
pub(crate) fn value_to_sql(value: &Value) -> SqlValue {
    match value {
        Value::Null => SqlValue::Null,
        Value::Bool(v) => SqlValue::Integer(i64::from(*v)),
        Value::Int(v) => SqlValue::Integer(*v),
        Value::Float(v) => SqlValue::Real(*v),
        Value::String(v) => SqlValue::Text(v.clone()),
        Value::Bytes(v) => SqlValue::Blob(v.clone()),
        Value::Date(v) => SqlValue::Text(v.to_string()),
        Value::Time(v) => SqlValue::Text(v.to_string()),
        Value::DateTime(v) => SqlValue::Text(v.to_string()),
        Value::Timestamp(v) => SqlValue::Text(v.to_rfc3339()),
        Value::Uuid(v) => SqlValue::Text(v.to_string()),
        Value::Json(v) => SqlValue::Text(v.to_string()),
        Value::Unknown(v) => SqlValue::Text(v.clone()),
    }
}

/// Convert a borrowed `rusqlite` value reference into the engine-agnostic
/// representation.
pub(crate) fn value_from_ref(value: ValueRef<'_>) -> Value {
    match value {
        ValueRef::Null => Value::Null,
        ValueRef::Integer(v) => Value::Int(v),
        ValueRef::Real(v) => Value::Float(v),
        ValueRef::Text(bytes) => match std::str::from_utf8(bytes) {
            Ok(s) => Value::String(s.to_owned()),
            Err(_) => Value::Bytes(bytes.to_vec()),
        },
        ValueRef::Blob(bytes) => Value::Bytes(bytes.to_vec()),
    }
}
