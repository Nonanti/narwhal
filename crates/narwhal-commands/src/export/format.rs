//! Export format enum and qualified-name helper.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportFormat {
    /// RFC 4180 CSV with CRLF line endings, header row, fields quoted
    /// when they contain delimiters/quotes.
    Csv,
    /// Array-of-objects JSON, one object per row, column names as keys.
    Json,
    /// Tab-separated values — no quoting, tabs / newlines in cells
    /// replaced with spaces. Pragmatic for shell pipelines, not a
    /// formal standard.
    Tsv,
    /// Human-readable ASCII grid for terminal output. Variable-width
    /// columns; not intended for machine consumption.
    Table,
    /// `INSERT INTO ... VALUES (...)` statements, requires a known
    /// source table.
    Insert,
}

impl ExportFormat {
    pub fn from_token(token: &str) -> Option<Self> {
        match token.to_ascii_lowercase().as_str() {
            "csv" => Some(Self::Csv),
            "json" => Some(Self::Json),
            "tsv" => Some(Self::Tsv),
            "table" | "tbl" => Some(Self::Table),
            "insert" | "sql" => Some(Self::Insert),
            _ => None,
        }
    }

    pub const fn default_extension(self) -> &'static str {
        match self {
            Self::Csv => "csv",
            Self::Json => "json",
            Self::Tsv => "tsv",
            Self::Table => "txt",
            Self::Insert => "sql",
        }
    }
}

/// A qualified table name of the form `schema.table` or just `table`.
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
