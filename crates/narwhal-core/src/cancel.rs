// Trait definitions intentionally keep explicit `'a` lifetimes on the
// dyn-safe sibling methods: every borrowed parameter shares the same
// lifetime as the returned `BoxFuture`, which elision cannot express
// (multi-input borrows would each get an independent anonymous
// lifetime).
#![allow(clippy::needless_lifetimes, clippy::elidable_lifetime_names)]

use crate::future::BoxFuture;
use std::future::Future;

use crate::error::Result;

/// Handle that requests cancellation of an in-flight query.
///
/// A handle is acquired from [`crate::Connection::cancel_handle`] before
/// dispatching the query and may be invoked from any task. Cancellation is
/// best-effort and engine-dependent; calling `cancel` after the query has
/// already completed is a no-op.
///
/// Native `async fn` in trait shape; for trait-object use (`Box<dyn ...>`)
/// see [`DynCancelHandle`].
pub trait CancelHandle: Send + Sync {
    fn cancel(&self) -> impl Future<Output = Result<()>> + Send;
}

/// Dyn-safe sibling of [`CancelHandle`].
pub trait DynCancelHandle: Send + Sync {
    fn cancel<'a>(&'a self) -> BoxFuture<'a, Result<()>>;
}

impl<T> DynCancelHandle for T
where
    T: CancelHandle + 'static,
{
    fn cancel<'a>(&'a self) -> BoxFuture<'a, Result<()>> {
        Box::pin(<Self as CancelHandle>::cancel(self))
    }
}
