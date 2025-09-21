use chrono::{DateTime, NaiveDate, NaiveDateTime, NaiveTime, Utc};
use serde::{Deserialize, Serialize};

/// Engine-agnostic representation of a database value.
///
/// Drivers convert their native types into [`Value`] when reading rows and
/// in the opposite direction when binding parameters. Values that cannot be
/// expressed in one of the structured variants are preserved in
/// [`Value::Unknown`] as their textual rendering.
#[derive(Debug, Clone, Serialize, Deserialize)]
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
    pub fn render(&self) -> String {
        match self {
            Value::Null => "NULL".into(),
            Value::Bool(b) => b.to_string(),
            Value::Int(i) => i.to_string(),
            Value::Float(f) => f.to_string(),
            Value::String(s) => s.clone(),
            Value::Bytes(b) => format!("<{} bytes>", b.len()),
            Value::Date(d) => d.to_string(),
            Value::Time(t) => t.to_string(),
            Value::DateTime(dt) => dt.to_string(),
            Value::Timestamp(ts) => ts.to_rfc3339(),
            Value::Uuid(u) => u.to_string(),
            Value::Json(v) => v.to_string(),
            Value::Unknown(s) => s.clone(),
        }
    }
}

impl std::fmt::Display for Value {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.render())
    }
}
