//! Workspace-wide future-shape type alias.
//!
//! The dyn-safe sibling traits ([`crate::DynConnection`],
//! [`crate::DynDatabaseDriver`], [`crate::DynRowStream`],
//! [`crate::DynCancelHandle`]) all return the same boxed-future
//! shape:
//!
//! ```rust
//! # use std::future::Future;
//! # use std::pin::Pin;
//! type X<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;
//! ```
//!
//! Spelling that out at every method site bloats each trait by
//! ~16 lines and pushes signatures past `clippy::type_complexity`
//! without telling the reader anything new. [`BoxFuture`] gives
//! every site a single, named expression.

use std::future::Future;
use std::pin::Pin;

/// Boxed, pinned, `Send` future. The canonical return type of every
/// dyn-safe async method in the workspace.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;
