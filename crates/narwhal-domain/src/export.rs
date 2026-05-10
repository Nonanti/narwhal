//! Pure types shared between the export pipeline and the result-pane
//! state that records where a `Rows` result came from.
//!
//! Note: this `QualifiedName` is *not* the same shape as
//! [`crate::QualifiedName`] — that one (in `crate::relation`) always
//! carries a schema; this one allows it to be absent, because some
//! databases (`SQLite`, single-schema `MySQL` setups, …) don't have a
//! meaningful schema qualifier. We keep them apart on purpose so a
//! caller can't accidentally feed one into the other.

/// A qualified table name of the form `schema.table` or just
/// `table`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QualifiedName {
    pub schema: Option<String>,
    pub table: String,
}

impl std::fmt::Display for QualifiedName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.schema {
            Some(s) => write!(f, "{s}.{}", self.table),
            None => write!(f, "{}", self.table),
        }
    }
}
