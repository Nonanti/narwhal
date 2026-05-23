//! Errors surfaced by the export pipeline.

#[derive(Debug, thiserror::Error)]
pub enum ExportError {
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),
    #[error("serialisation error: {0}")]
    Serialise(String),
    #[error(
        "INSERT export requires a known source table; the query did not target a single table"
    )]
    NoSourceTable,
}
