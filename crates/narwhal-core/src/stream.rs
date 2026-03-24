// Trait definitions intentionally keep explicit `'a` lifetimes on the
// dyn-safe sibling methods: every borrowed parameter shares the same
// lifetime as the returned `BoxFuture`, which elision cannot express
// (multi-input borrows would each get an independent anonymous
// lifetime).
#![allow(clippy::needless_lifetimes, clippy::elidable_lifetime_names)]

use crate::future::BoxFuture;
use std::future::Future;

use crate::error::Result;
use crate::schema::{ColumnHeader, Row};

/// Asynchronous, row-by-row view of a query result.
///
/// Streams avoid materialising the entire result in memory and are the
/// preferred execution path for unbounded or interactively browsed result
/// sets.
///
/// Native `async fn` in trait shape; for trait-object use
/// (`Box<dyn ...>`) see [`DynRowStream`]. Drivers implement this trait;
/// the [`DynRowStream`] blanket impl boxes the returned futures so the
/// rest of the workspace can shuffle streams through trait objects.
pub trait RowStream: Send {
    /// Column headers describing the shape of every row produced by the
    /// stream.
    fn columns(&self) -> &[ColumnHeader];

    /// Advance the stream and return the next row, or `None` at the end.
    fn next_row(&mut self) -> impl Future<Output = Result<Option<Row>>> + Send;

    /// Release server-side resources held by the stream (cursors, portals).
    fn close(self: Box<Self>) -> impl Future<Output = Result<()>> + Send;
}

/// Dyn-safe sibling of [`RowStream`].
pub trait DynRowStream: Send {
    fn columns(&self) -> &[ColumnHeader];

    fn next_row<'a>(&'a mut self) -> BoxFuture<'a, Result<Option<Row>>>;

    fn close(self: Box<Self>) -> BoxFuture<'static, Result<()>>;
}

impl<T> DynRowStream for T
where
    T: RowStream + 'static,
{
    fn columns(&self) -> &[ColumnHeader] {
        <Self as RowStream>::columns(self)
    }

    fn next_row<'a>(&'a mut self) -> BoxFuture<'a, Result<Option<Row>>> {
        Box::pin(<Self as RowStream>::next_row(self))
    }

    fn close(self: Box<Self>) -> BoxFuture<'static, Result<()>> {
        Box::pin(<Self as RowStream>::close(self))
    }
}
