//! Convert [`narwhal_core::Value`] into the JSON shape an AI agent expects.
//!
//! The default `#[derive(Serialize)]` impl on the enum produces tagged
//! objects (`{"Int": 42}`, `{"String": "hi"}`) which are technically
//! correct but force the agent to learn narwhal-specific tags before it
//! can read a query result. We want the obvious mapping instead:
//!
//! | `Value` variant | JSON                                  |
//! |-----------------|---------------------------------------|
//! | `Null`          | `null`                                |
//! | `Bool`          | `true` / `false`                      |
//! | `Int`           | number                                |
//! | `Float`         | number (or `null` for `NaN` / `±Inf`) |
//! | `String`        | string                                |
//! | `Bytes`         | `{ "$bytes_base64": "..." }`          |
//! | `Date`          | `"YYYY-MM-DD"`                        |
//! | `Time`          | `"HH:MM:SS[.fff]"`                    |
//! | `DateTime`      | `"YYYY-MM-DDTHH:MM:SS[.fff]"`         |
//! | `Timestamp`     | RFC 3339                              |
//! | `Uuid`          | `"00000000-0000-0000-0000-..."`       |
//! | `Json`          | embedded as-is                        |
//! | `Unknown`       | string                                |
//!
//! Binary blobs are wrapped in a sentinel object instead of a bare base64
//! string so an agent that round-trips the value can tell "this was bytes,
//! not text". JSON has no native byte type; the sentinel is the standard
//! workaround across MCP server implementations.

use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use narwhal_core::Value;
use serde_json::{json, Value as Json};

/// Render a single [`Value`] as JSON suitable for an LLM agent.
pub fn value_to_json(v: &Value) -> Json {
    match v {
        Value::Null => Json::Null,
        Value::Bool(b) => Json::Bool(*b),
        Value::Int(i) => Json::Number((*i).into()),
        Value::Float(f) => {
            // serde_json refuses to serialize NaN/Inf in `Number`; surfacing
            // them as `null` is the standard JSON compromise (matches what
            // Postgres' `to_jsonb(double precision)` does on a NaN row).
            serde_json::Number::from_f64(*f).map_or(Json::Null, Json::Number)
        }
        Value::String(s) => Json::String(s.clone()),
        Value::Bytes(b) => json!({ "$bytes_base64": B64.encode(b) }),
        Value::Date(d) => Json::String(d.to_string()),
        Value::Time(t) => Json::String(t.to_string()),
        Value::DateTime(dt) => Json::String(dt.to_string()),
        Value::Timestamp(ts) => Json::String(ts.to_rfc3339()),
        Value::Uuid(u) => Json::String(u.to_string()),
        Value::Json(j) => j.clone(),
        Value::Unknown(s) => Json::String(s.clone()),
        // M14 added `#[non_exhaustive]` — fall back to the textual rendering
        // so a future Value variant degrades to a string instead of panicking.
        _ => Json::String(v.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{NaiveDate, NaiveTime};

    #[test]
    fn primitives_map_cleanly() {
        assert_eq!(value_to_json(&Value::Null), json!(null));
        assert_eq!(value_to_json(&Value::Bool(true)), json!(true));
        assert_eq!(value_to_json(&Value::Int(42)), json!(42));
        assert_eq!(value_to_json(&Value::Float(3.5)), json!(3.5));
        assert_eq!(value_to_json(&Value::String("hi".into())), json!("hi"));
    }

    #[test]
    fn nan_and_infinity_become_null() {
        assert_eq!(value_to_json(&Value::Float(f64::NAN)), Json::Null);
        assert_eq!(value_to_json(&Value::Float(f64::INFINITY)), Json::Null);
        assert_eq!(value_to_json(&Value::Float(f64::NEG_INFINITY)), Json::Null);
    }

    #[test]
    fn bytes_are_base64_wrapped() {
        let out = value_to_json(&Value::Bytes(vec![1, 2, 3]));
        // base64("\x01\x02\x03") == "AQID"
        assert_eq!(out, json!({ "$bytes_base64": "AQID" }));
    }

    #[test]
    fn date_time_use_iso_format() {
        let d = NaiveDate::from_ymd_opt(2024, 1, 2).expect("valid date");
        assert_eq!(value_to_json(&Value::Date(d)), json!("2024-01-02"));
        let t = NaiveTime::from_hms_opt(13, 45, 30).expect("valid time");
        assert_eq!(value_to_json(&Value::Time(t)), json!("13:45:30"));
    }

    #[test]
    fn embedded_json_is_passed_through_unwrapped() {
        let inner = json!({"a": [1, 2, 3]});
        assert_eq!(
            value_to_json(&Value::Json(inner.clone())),
            inner,
            "embedded JSON must not be re-wrapped"
        );
    }
}
