//! Connection pool for narwhal drivers.
//!
//! The pool is keyed by a single [`narwhal_core::ConnectionConfig`] and a
//! resolved credential. Connections are created lazily up to the configured
//! ceiling and recycled on drop. Health checks are performed before a
//! connection is handed out so that consumers never observe a broken
//! session.

#![forbid(unsafe_code)]

pub mod pool;

pub use pool::{Pool, PoolConfig, PoolError, PooledConnection};
