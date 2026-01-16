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
use thiserror::Error;

/// Errors returned by [`json_to_value`].
///
/// The single variant carries a `&'static str` hint pointing at the
/// offending kind so the surfaced message is precise: "expected integer,
/// got string". Agents recover by re-issuing the call with corrected
/// arguments.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum FromJsonError {
    #[error("cannot map JSON {kind} to a SQL bind parameter at position {index}")]
    Unsupported { index: usize, kind: &'static str },
    #[error(
        "bytes parameter at position {index} is malformed: \
         expected `{{\"$bytes_base64\": \"<base64>\"}}`"
    )]
    MalformedBytes { index: usize },
}

/// Convert a JSON value back into a narwhal [`Value`] suitable for
/// binding as a SQL parameter.
///
/// The mapping is the inverse of [`value_to_json`] with a few practical
/// liberties: a JSON string that happens to look like an ISO date is
/// **not** auto-coerced to `Value::Date`, because the agent might have
/// meant a literal string. Use the `$bytes_base64` envelope to round-trip
/// blobs; everything else uses the obvious mapping.
///
/// | JSON                                | narwhal `Value`           |
/// |-------------------------------------|---------------------------|
/// | `null`                              | `Null`                    |
/// | `true` / `false`                    | `Bool`                    |
/// | integer                             | `Int`                     |
/// | float                               | `Float`                   |
/// | `"..."`                             | `String`                  |
/// | `{"$bytes_base64": "..."}`          | `Bytes`                   |
/// | array / arbitrary object            | `Json` (driver decides)   |
///
/// `index` is the zero-based position of the parameter inside the
/// `params` array; it is threaded into the error so the agent can fix
/// the right slot without guessing.
pub fn json_to_value(index: usize, j: &Json) -> Result<Value, FromJsonError> {
    match j {
        Json::Null => Ok(Value::Null),
        Json::Bool(b) => Ok(Value::Bool(*b)),
        Json::Number(n) => {
            if let Some(i) = n.as_i64() {
                Ok(Value::Int(i))
            } else if let Some(f) = n.as_f64() {
                Ok(Value::Float(f))
            } else {
                // Number that fits neither i64 nor f64 (u64 > i64::MAX).
                // Fall back to the textual rendering so we don't lie
                // about the type — most drivers accept this as text and
                // re-parse it correctly.
                Ok(Value::Unknown(n.to_string()))
            }
        }
        Json::String(s) => Ok(Value::String(s.clone())),
        Json::Object(map) => {
            // The `$bytes_base64` envelope is the only structured form
            // we recognise specially; anything else is passed through as
            // a JSON Value so a JSONB / json column on the driver can
            // accept it.
            if let Some(b64) = map.get("$bytes_base64").and_then(Json::as_str) {
                let bytes = B64
                    .decode(b64)
                    .map_err(|_| FromJsonError::MalformedBytes { index })?;
                return Ok(Value::Bytes(bytes));
            }
            Ok(Value::Json(j.clone()))
        }
        Json::Array(_) => Ok(Value::Json(j.clone())),
    }
}

/// Convenience wrapper: convert a slice of JSON values into a `Vec` of
/// narwhal values, stopping at the first error. Used by `run_query` /
/// `explain_query` to decode the `params` argument array.
pub fn json_array_to_values(values: &[Json]) -> Result<Vec<Value>, FromJsonError> {
    values
        .iter()
        .enumerate()
        .map(|(i, v)| json_to_value(i, v))
        .collect()
}

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

    // ----- json_to_value -----

    #[test]
    fn json_to_value_primitives() {
        assert!(matches!(
            json_to_value(0, &json!(null)).unwrap(),
            Value::Null
        ));
        assert!(matches!(
            json_to_value(0, &json!(true)).unwrap(),
            Value::Bool(true)
        ));
        assert!(matches!(
            json_to_value(0, &json!(42)).unwrap(),
            Value::Int(42)
        ));
        assert!(matches!(
            json_to_value(0, &json!(2.5)).unwrap(),
            Value::Float(_)
        ));
        match json_to_value(0, &json!("hi")).unwrap() {
            Value::String(s) => assert_eq!(s, "hi"),
            other => panic!("expected String, got {other:?}"),
        }
    }

    #[test]
    fn json_to_value_decodes_bytes_envelope() {
        // base64("\x01\x02\x03") == "AQID"
        let v = json_to_value(0, &json!({"$bytes_base64": "AQID"})).unwrap();
        match v {
            Value::Bytes(b) => assert_eq!(b, vec![1, 2, 3]),
            other => panic!("expected Bytes, got {other:?}"),
        }
    }

    #[test]
    fn json_to_value_malformed_bytes_envelope_errors() {
        let err = json_to_value(2, &json!({"$bytes_base64": "!!!"})).unwrap_err();
        match err {
            FromJsonError::MalformedBytes { index } => assert_eq!(index, 2),
            other => panic!("expected MalformedBytes, got {other:?}"),
        }
    }

    #[test]
    fn json_to_value_object_without_envelope_becomes_json_value() {
        let v = json_to_value(0, &json!({"a": 1})).unwrap();
        match v {
            Value::Json(inner) => assert_eq!(inner, json!({"a": 1})),
            other => panic!("expected Json, got {other:?}"),
        }
    }

    #[test]
    fn json_array_to_values_short_circuits_on_first_error() {
        let arr = vec![json!(1), json!({"$bytes_base64": "!!!"}), json!(2)];
        let err = json_array_to_values(&arr).unwrap_err();
        match err {
            FromJsonError::MalformedBytes { index } => assert_eq!(index, 1),
            other => panic!("expected MalformedBytes, got {other:?}"),
        }
    }

    #[test]
    fn json_array_round_trip() {
        // A subset of value_to_json shapes that survive the round-trip
        // intact. Some shapes (datetime, uuid) don't round-trip because
        // they serialise to JSON strings on the way out; the agent has
        // to opt into specific types on bind via dialect literals.
        let values = [
            Value::Null,
            Value::Bool(false),
            Value::Int(7),
            Value::String("x".into()),
            Value::Bytes(vec![9, 8, 7]),
        ];
        let json: Vec<Json> = values.iter().map(value_to_json).collect();
        let back = json_array_to_values(&json).unwrap();
        assert!(matches!(back[0], Value::Null));
        assert!(matches!(back[1], Value::Bool(false)));
        assert!(matches!(back[2], Value::Int(7)));
        match &back[3] {
            Value::String(s) => assert_eq!(s, "x"),
            other => panic!("expected String, got {other:?}"),
        }
        match &back[4] {
            Value::Bytes(b) => assert_eq!(b, &[9, 8, 7]),
            other => panic!("expected Bytes, got {other:?}"),
        }
    }
}
