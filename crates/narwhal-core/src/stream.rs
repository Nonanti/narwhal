use async_trait::async_trait;

use crate::error::Result;
use crate::schema::{ColumnHeader, Row};

/// Asynchronous, row-by-row view of a query result.
///
/// Streams avoid materialising the entire result in memory and are the
/// preferred execution path for unbounded or interactively browsed result
/// sets. The trait is object-safe; concrete driver implementations live in
/// the corresponding driver crate.
#[async_trait]
pub trait RowStream: Send {
    /// Column headers describing the shape of every row produced by the
    /// stream.
    fn columns(&self) -> &[ColumnHeader];

    /// Advance the stream and return the next row, or `None` at the end.
    async fn next_row(&mut self) -> Result<Option<Row>>;

    /// Release server-side resources held by the stream (cursors, portals).
    async fn close(self: Box<Self>) -> Result<()>;
}
