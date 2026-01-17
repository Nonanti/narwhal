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
    pub const fn is_null(&self) -> bool {
        matches!(self, Self::Null)
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
            Self::Null => f.write_str("NULL"),
            Self::Bool(b) => write!(f, "{b}"),
            Self::Int(i) => write!(f, "{i}"),
            Self::Float(v) => write!(f, "{v}"),
            Self::String(s) => f.write_str(s),
            Self::Bytes(b) => write!(f, "<{} bytes>", b.len()),
            Self::Date(d) => write!(f, "{d}"),
            Self::Time(t) => write!(f, "{t}"),
            Self::DateTime(dt) => write!(f, "{dt}"),
            Self::Timestamp(ts) => f.write_str(&ts.to_rfc3339()),
            Self::Uuid(u) => write!(f, "{u}"),
            Self::Json(v) => write!(f, "{v}"),
            Self::Unknown(s) => f.write_str(s),
        }
    }
}
