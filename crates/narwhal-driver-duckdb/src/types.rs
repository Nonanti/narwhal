//! Conversion layer between [`narwhal_core::Value`] and `duckdb`.
//!
//! `DuckDB` has a richer native type system than `SQLite` (hugeints, decimals,
//! intervals, lists, structs, maps, …). For now we map the common cases
//! to engine-agnostic variants and fall back to a string rendering for
//! everything else — that's enough to *display* exotic results, which is
//! what 95% of TUI clients need to do anyway. Round-tripping composite
//! types is intentionally not promised.

use duckdb::types::{TimeUnit, Value as DuckValue, ValueRef};
use narwhal_core::Value;

/// Convert a [`Value`] into an owned `DuckDB` value suitable for parameter
/// binding. Engine-specific types (Date, Timestamp, …) are encoded as
/// strings so the round-trip remains lossless even when the column is a
/// VARCHAR; `DuckDB` casts on its end.
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
        // Forward-compatible: render any future variant as its Debug form.
        other => DuckValue::Text(format!("{other:?}")),
    }
}

/// Convert a borrowed `DuckDB` value reference into the engine-agnostic
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
        // M12: Date32/Time64/Timestamp/Interval now render as proper
        // chrono types instead of opaque "date(N)" strings.
        ValueRef::Date32(days) => {
            // days is days since 1970-01-01 in the Unix epoch.
            // chrono's from_num_days_from_ce_opt uses the CE epoch
            // (day 1 = 0001-01-01). Unix epoch day 0 = CE day 719_163.
            chrono::NaiveDate::from_num_days_from_ce_opt(days + 719_163).map_or_else(|| Value::String(format!("date({days})")), Value::Date)
        }
        ValueRef::Time64(unit, ticks) => {
            let ns = scaled_ns(unit, ticks);
            let secs = (ns / 1_000_000_000) as u32;
            let sub_ns = (ns % 1_000_000_000) as u32;
            chrono::NaiveTime::from_num_seconds_from_midnight_opt(secs, sub_ns).map_or_else(|| Value::String(format!("time({ns}ns)")), Value::Time)
        }
        ValueRef::Timestamp(unit, ticks) => {
            let ns = scaled_ns(unit, ticks);
            let secs = ns / 1_000_000_000;
            let sub_ns = (ns % 1_000_000_000) as u32;
            chrono::DateTime::<chrono::Utc>::from_timestamp(secs, sub_ns).map_or_else(|| Value::String(format!("timestamp({ns}ns)")), Value::Timestamp)
        }
        ValueRef::Interval {
            months,
            days,
            nanos,
        } => {
            // Format as ISO 8601 duration-like string.
            // P[n]Y[n]M[n]DT[n]H[n]M[n]S
            let years = months / 12;
            let rem_months = months % 12;
            let hours = nanos / 3_600_000_000_000;
            let rem_ns = nanos % 3_600_000_000_000;
            let minutes = rem_ns / 60_000_000_000;
            let rem_ns2 = rem_ns % 60_000_000_000;
            let seconds = rem_ns2 / 1_000_000_000;
            let sub_ns = rem_ns2 % 1_000_000_000;
            let mut s = String::from("P");
            if years != 0 {
                s.push_str(&format!("{years}Y"));
            }
            if rem_months != 0 {
                s.push_str(&format!("{rem_months}M"));
            }
            if days != 0 {
                s.push_str(&format!("{days}D"));
            }
            if hours != 0 || minutes != 0 || seconds != 0 || sub_ns != 0 {
                s.push('T');
                if hours != 0 {
                    s.push_str(&format!("{hours}H"));
                }
                if minutes != 0 {
                    s.push_str(&format!("{minutes}M"));
                }
                if seconds != 0 || sub_ns != 0 {
                    s.push_str(&format!("{seconds}"));
                    if sub_ns != 0 {
                        s.push_str(format!(".{sub_ns:09}").trim_end_matches('0'));
                    }
                    s.push('S');
                }
            }
            if s == "P" {
                s.push_str("0D");
            }
            Value::String(s)
        }
        // Anything we don't recognise (Enum, List, Struct, Map, Union, …)
        // gets rendered through DuckDB's owned-value Debug impl, which is
        // already a reasonable human-readable form.
        other => Value::String(format!("{:?}", other.to_owned())),
    }
}

/// Convert a `TimeUnit` + ticks pair into nanoseconds.
const fn scaled_ns(unit: TimeUnit, ticks: i64) -> i64 {
    match unit {
        TimeUnit::Second => ticks * 1_000_000_000,
        TimeUnit::Millisecond => ticks * 1_000_000,
        TimeUnit::Microsecond => ticks * 1_000,
        TimeUnit::Nanosecond => ticks,
    }
}
