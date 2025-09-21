use async_trait::async_trait;

use crate::error::Result;

/// Handle that requests cancellation of an in-flight query.
///
/// A handle is acquired from [`crate::Connection::cancel_handle`] before
/// dispatching the query and may be invoked from any task. Cancellation is
/// best-effort and engine-dependent; calling `cancel` after the query has
/// already completed is a no-op.
#[async_trait]
pub trait CancelHandle: Send + Sync {
    async fn cancel(&self) -> Result<()>;
}
