//! Export format enum, options + qualified-name helper.

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
    /// T1-T4-B: Apache Parquet columnar format. The writer materialises
    /// the entire result in memory before encoding (streaming Parquet
    /// is a v2.2+ concern).
    Parquet,
    /// T1-T4-B: GitHub-Flavoured Markdown table — pipe-separated cells
    /// with a header separator line. Rows truncate at
    /// [`MarkdownOptions::row_limit`] by default; pass `--no-truncate`
    /// to dump everything.
    Markdown,
}

impl ExportFormat {
    pub fn from_token(token: &str) -> Option<Self> {
        match token.to_ascii_lowercase().as_str() {
            "csv" => Some(Self::Csv),
            "json" => Some(Self::Json),
            "tsv" => Some(Self::Tsv),
            "table" | "tbl" => Some(Self::Table),
            "insert" | "sql" => Some(Self::Insert),
            // T1-T4-B: `pq` is the common shorthand on the Python /
            // pandas side; accept it so muscle memory works.
            "parquet" | "pq" => Some(Self::Parquet),
            // `md` is the GFM extension; both spellings land here.
            "markdown" | "md" => Some(Self::Markdown),
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
            Self::Parquet => "parquet",
            Self::Markdown => "md",
        }
    }
}

/// Compression codec for the Parquet writer.
///
/// `Snappy` is the default because it is the de-facto Parquet
/// compression: fast to encode, fast to decode, supported by every
/// downstream reader without an extra feature flag. `Zstd` produces
/// substantially smaller files at a modest CPU cost — use it when
/// shipping the file across a network or storing long-term. `None`
/// is useful for benchmarks and for cases where the file is already
/// stored on a compressed filesystem.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ParquetCompression {
    #[default]
    Snappy,
    Zstd,
    None,
}

impl ParquetCompression {
    /// Parse a compression token from the CLI (`--compression zstd`).
    /// Case-insensitive. `none` / `uncompressed` both map to `None`
    /// so users coming from either ecosystem find the right name.
    pub fn from_token(token: &str) -> Option<Self> {
        match token.to_ascii_lowercase().as_str() {
            "snappy" | "snap" => Some(Self::Snappy),
            "zstd" | "zst" => Some(Self::Zstd),
            "none" | "uncompressed" => Some(Self::None),
            _ => None,
        }
    }
}

/// Options for the Markdown writer.
///
/// Defaults match the "I want a tidy GFM table I can paste into a PR"
/// workflow: cap at 1000 rows and append a truncation marker so the
/// reader knows more rows exist. `--no-truncate` flips
/// [`MarkdownOptions::row_limit`] to `None`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MarkdownOptions {
    /// `None` means write every row. `Some(n)` caps the output at the
    /// first `n` rows and appends a truncation marker.
    pub row_limit: Option<usize>,
}

impl Default for MarkdownOptions {
    fn default() -> Self {
        Self {
            row_limit: Some(1000),
        }
    }
}

/// Format-specific options bag threaded through [`super::export_rows`].
///
/// The struct is `#[non_exhaustive]` so we can add Parquet row-group
/// size, Markdown styling, etc. without breaking external callers.
/// Construct via [`ExportOptions::default`] and mutate the fields you
/// care about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub struct ExportOptions {
    pub parquet_compression: ParquetCompression,
    pub markdown: MarkdownOptions,
}

impl ExportOptions {
    /// Shortcut for the common "Parquet with a specific codec" case.
    /// Keeps test code readable without sprinkling `..Default::default()`.
    #[must_use]
    pub const fn parquet(compression: ParquetCompression) -> Self {
        Self {
            parquet_compression: compression,
            markdown: MarkdownOptions {
                row_limit: Some(1000),
            },
        }
    }

    /// Shortcut for the "Markdown without truncation" case.
    #[must_use]
    pub const fn markdown_full() -> Self {
        Self {
            parquet_compression: ParquetCompression::Snappy,
            markdown: MarkdownOptions { row_limit: None },
        }
    }
}

// `QualifiedName` moved to `narwhal_domain::export` so the result-pane
// state in `narwhal-domain` can name it without pulling
// `narwhal-commands` along. Re-exported here so the
// `narwhal_commands::export::QualifiedName` import path keeps working.
pub use narwhal_domain::export::QualifiedName;
