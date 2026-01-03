//! Plugin SQL executor wiring extracted from `core.rs` (L21).
//!
//! `AppCore` injects [`AppPluginExecutor`] into every Lua plugin so the
//! script's `narwhal.sql_run` calls hit whatever connection the user has
//! active *right now*. The shared [`PluginConnectionState`] lets
//! `:open`/`:close`/`:begin`/`:commit` retarget or pause plugin SQL
//! without rebuilding plugin objects.
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use narwhal_plugin::{PluginError, PluginResult, SqlExecutor};
use narwhal_pool::Pool;

/// Shared state read by every plugin SQL executor on every
/// `narwhal.sql_run` call.
///
/// Owned by [`super::AppCore`] inside an `Arc<std::sync::Mutex<_>>` so:
///
/// * opening/closing a session can retarget plugin SQL transparently
///   without rebuilding plugin objects;
/// * `:begin`/`:commit`/`:rollback` can flip the in-transaction flag so
///   the executor refuses to run during a pinned transaction (a fresh
///   pool connection wouldn't see uncommitted state — silent
///   correctness bug otherwise);
/// * the plain `std::sync::Mutex` is fine because every access is short
///   (clone the pool out, drop the guard) and never spans an `.await`.
#[derive(Default)]
pub(crate) struct PluginConnectionState {
    pub(crate) pool: Option<Pool>,
    pub(crate) in_transaction: bool,
}

/// SQL executor injected into every Lua plugin loaded by AppCore.
///
/// Reads [`PluginConnectionState`] on every call so the script always
/// targets the *currently active* connection. Refuses to run while a
/// `:begin` transaction is open — see [`PluginConnectionState`] for
/// why.
///
/// ### Memory footprint
///
/// `narwhal.sql_run` materialises the whole result set in memory before
/// returning to Lua. Scripts that query unbounded tables can OOM the
/// process; recommend `LIMIT` in the user-facing docs. Streaming
/// support is a future addition.
pub(super) struct AppPluginExecutor {
    pub(super) state: Arc<Mutex<PluginConnectionState>>,
}

#[async_trait]
impl SqlExecutor for AppPluginExecutor {
    async fn run(&self, sql: &str) -> PluginResult<narwhal_core::QueryResult> {
        // Grab a snapshot of the state and drop the guard *before* we
        // touch any async API.
        let (pool, in_tx) = {
            let guard = self
                .state
                .lock()
                .map_err(|e| PluginError::Runtime(format!("plugin state poisoned: {e}")))?;
            (guard.pool.clone(), guard.in_transaction)
        };
        if in_tx {
            return Err(PluginError::Runtime(
                "narwhal.sql_run is unavailable while a :begin transaction is open".into(),
            ));
        }
        let pool = pool.ok_or_else(|| PluginError::Runtime("no active connection".into()))?;
        let mut conn = pool
            .acquire()
            .await
            .map_err(|e| PluginError::Runtime(format!("could not acquire connection: {e}")))?;
        conn.execute(sql, &[])
            .await
            .map_err(|e| PluginError::Runtime(format!("execute: {e}")))
    }
}

