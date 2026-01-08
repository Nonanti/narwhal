use chrono::{DateTime, NaiveDate, NaiveDateTime, NaiveTime, Utc};
use serde::{Deserialize, Serialize};

/// Engine-agnostic representation of a database value.
///
/// Drivers convert their native types into [`Value`] when reading rows and
/// in the opposite direction when binding parameters. Values that cannot be
/// expressed in one of the structured variants are preserved in
/// [`Value::Unknown`] as their textual rendering.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub enum Value {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    String(String),
    Bytes(Vec<u8>),
    Date(NaiveDate),
    Time(NaiveTime),
    DateTime(NaiveDateTime),
    Timestamp(DateTime<Utc>),
    Uuid(uuid::Uuid),
    Json(serde_json::Value),
    Unknown(String),
}

impl Value {
    pub fn is_null(&self) -> bool {
        matches!(self, Value::Null)
    }

    /// Render the value as a plain string suitable for display in a grid.
    ///
    /// Delegates to [`std::fmt::Display`] which writes straight to the
    /// formatter — no intermediate allocation for the integer/float/date
    /// paths (L1).
    pub fn render(&self) -> String {
        self.to_string()
    }
}

impl std::fmt::Display for Value {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Value::Null => f.write_str("NULL"),
            Value::Bool(b) => write!(f, "{b}"),
            Value::Int(i) => write!(f, "{i}"),
            Value::Float(v) => write!(f, "{v}"),
            Value::String(s) => f.write_str(s),
            Value::Bytes(b) => write!(f, "<{} bytes>", b.len()),
            Value::Date(d) => write!(f, "{d}"),
            Value::Time(t) => write!(f, "{t}"),
            Value::DateTime(dt) => write!(f, "{dt}"),
            Value::Timestamp(ts) => f.write_str(&ts.to_rfc3339()),
            Value::Uuid(u) => write!(f, "{u}"),
            Value::Json(v) => write!(f, "{v}"),
            Value::Unknown(s) => f.write_str(s),
        }
    }
}
