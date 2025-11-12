//! Conversion layer between [`narwhal_core::Value`] and `duckdb`.
//!
//! DuckDB has a richer native type system than SQLite (hugeints, decimals,
//! intervals, lists, structs, maps, …). For now we map the common cases
//! to engine-agnostic variants and fall back to a string rendering for
//! everything else — that's enough to *display* exotic results, which is
//! what 95% of TUI clients need to do anyway. Round-tripping composite
//! types is intentionally not promised.

use duckdb::types::{TimeUnit, Value as DuckValue, ValueRef};
use narwhal_core::Value;

/// Convert a [`Value`] into an owned DuckDB value suitable for parameter
/// binding. Engine-specific types (Date, Timestamp, …) are encoded as
/// strings so the round-trip remains lossless even when the column is a
/// VARCHAR; DuckDB casts on its end.
pub(crate) fn value_to_sql(value: &Value) -> DuckValue {
    match value {
        Value::Null => DuckValue::Null,
        Value::Bool(v) => DuckValue::Boolean(*v),
        Value::Int(v) => DuckValue::BigInt(*v),
        Value::Float(v) => DuckValue::Double(*v),
        Value::String(v) => DuckValue::Text(v.clone()),
        Value::Bytes(v) => DuckValue::Blob(v.clone()),
        Value::Date(v) => DuckValue::Text(v.to_string()),
        Value::Time(v) => DuckValue::Text(v.to_string()),
        Value::DateTime(v) => DuckValue::Text(v.to_string()),
        Value::Timestamp(v) => DuckValue::Text(v.to_rfc3339()),
        Value::Uuid(v) => DuckValue::Text(v.to_string()),
        Value::Json(v) => DuckValue::Text(v.to_string()),
        Value::Unknown(v) => DuckValue::Text(v.clone()),
    }
}

/// Convert a borrowed DuckDB value reference into the engine-agnostic
/// representation. Composite/temporal types that don't have a direct
/// counterpart in [`Value`] collapse to `Value::String` via their
/// debug/display form — good enough for read-only browsing.
pub(crate) fn value_from_ref(value: ValueRef<'_>) -> Value {
    match value {
        ValueRef::Null => Value::Null,
        ValueRef::Boolean(v) => Value::Bool(v),
        ValueRef::TinyInt(v) => Value::Int(i64::from(v)),
        ValueRef::SmallInt(v) => Value::Int(i64::from(v)),
        ValueRef::Int(v) => Value::Int(i64::from(v)),
        ValueRef::BigInt(v) => Value::Int(v),
        ValueRef::HugeInt(v) => Value::String(v.to_string()),
        ValueRef::UTinyInt(v) => Value::Int(i64::from(v)),
        ValueRef::USmallInt(v) => Value::Int(i64::from(v)),
        ValueRef::UInt(v) => Value::Int(i64::from(v)),
        ValueRef::UBigInt(v) => Value::String(v.to_string()),
        ValueRef::Float(v) => Value::Float(f64::from(v)),
        ValueRef::Double(v) => Value::Float(v),
        ValueRef::Decimal(d) => Value::String(d.to_string()),
        ValueRef::Text(bytes) => match std::str::from_utf8(bytes) {
            Ok(s) => Value::String(s.to_owned()),
            Err(_) => Value::Bytes(bytes.to_vec()),
        },
        ValueRef::Blob(bytes) => Value::Bytes(bytes.to_vec()),
        ValueRef::Date32(days) => Value::String(format!("date({days})")),
        ValueRef::Time64(unit, ticks) => Value::String(format!("time({})", scaled(unit, ticks))),
        ValueRef::Timestamp(unit, ticks) => {
            Value::String(format!("timestamp({})", scaled(unit, ticks)))
        }
        ValueRef::Interval {
            months,
            days,
            nanos,
        } => Value::String(format!("interval({months}m {days}d {nanos}ns)")),
        // Anything we don't recognise (Enum, List, Struct, Map, Union, …)
        // gets rendered through DuckDB's owned-value Debug impl, which is
        // already a reasonable human-readable form.
        other => Value::String(format!("{:?}", other.to_owned())),
    }
}

fn scaled(unit: TimeUnit, ticks: i64) -> String {
    let suffix = match unit {
        TimeUnit::Second => "s",
        TimeUnit::Millisecond => "ms",
        TimeUnit::Microsecond => "us",
        TimeUnit::Nanosecond => "ns",
    };
    format!("{ticks}{suffix}")
}
