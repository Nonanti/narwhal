//! Candidate types surfaced by the completion engine.

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum CompletionKind {
    /// Reserved SQL keyword (`SELECT`, `FROM`, …).
    Keyword,
    /// Table or view name.
    Table,
    /// Column belonging to a known table.
    Column,
    /// Built-in / aggregate function (`COUNT(`, `SUM(`, …).
    /// Inserted with the trailing `(` so the cursor lands inside the
    /// argument list ready for the user to type the column.
    Function,
}

/// Single completion candidate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Completion {
    pub text: String,
    pub kind: CompletionKind,
    /// Optional secondary text shown next to the completion (e.g. the
    /// schema for a table or the type for a column).
    pub detail: Option<String>,
}
