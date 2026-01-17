//! Transaction control extracted from `core.rs` (L21).
//!
//! Hosts `:begin`/`:commit`/`:rollback`/`:savepoint` and the
//! `with_txn_conn` helper that all guarded transaction operations share.
//! The pinned-connection lifecycle stays inside [`super::AppCore`]
//! because it touches plugin state, status bar and session at once.
use std::sync::Arc;

use narwhal_core::IsolationLevel;
use tokio::sync::Mutex;

use super::AppCore;
use crate::commands::IsolationArg;
use crate::session::{Session, TxnHandle};

/// Map a CLI-level isolation argument to the engine-agnostic level.
pub(super) const fn map_isolation(arg: IsolationArg) -> IsolationLevel {
    // IsolationArg is `#[non_exhaustive]` but lives in the same crate, so a
    // wildcard arm would be reported as unreachable. Match all variants.
    match arg {
        IsolationArg::ReadUncommitted => IsolationLevel::ReadUncommitted,
        IsolationArg::ReadCommitted => IsolationLevel::ReadCommitted,
        IsolationArg::RepeatableRead => IsolationLevel::RepeatableRead,
        IsolationArg::Serializable => IsolationLevel::Serializable,
    }
}

/// Human-readable label for a transaction isolation level.
pub(super) const fn isolation_label(level: IsolationLevel) -> &'static str {
    match level {
        IsolationLevel::ReadUncommitted => "read-uncommitted",
        IsolationLevel::ReadCommitted => "read-committed",
        IsolationLevel::RepeatableRead => "repeatable-read",
        IsolationLevel::Serializable => "serializable",
        // Future IsolationLevel variants surface as a generic label.
        _ => "isolation",
    }
}

impl AppCore {
    pub(super) fn begin_transaction(&mut self, isolation: Option<IsolationArg>) {
        if self.running {
            self.status.message = "a query is already running".into();
            return;
        }
        let Some(session) = self.session.as_mut() else {
            self.status.message = "no active connection".into();
            return;
        };
        if session.transaction.is_some() {
            self.status.message = "a transaction is already open".into();
            return;
        }
        let iso = isolation.map(map_isolation);
        let pool = session.pool.clone();
        let result: std::result::Result<narwhal_pool::PooledConnection, narwhal_core::Error> =
            tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(async move {
                    let mut conn = pool
                        .acquire()
                        .await
                        .map_err(|e| narwhal_core::Error::Connection(e.to_string()))?;
                    match iso {
                        Some(level) => conn.begin_with(level).await?,
                        None => conn.begin().await?,
                    }
                    Ok(conn)
                })
            });
        match result {
            Ok(conn) => {
                session.transaction = Some(TxnHandle {
                    conn: Arc::new(Mutex::new(conn)),
                    savepoints: Vec::new(),
                    isolation: iso,
                });
                // Mark the plugin executor so `narwhal.sql_run` refuses
                // to run while a transaction is open — a fresh pool
                // connection wouldn't see the uncommitted state and the
                // user would silently get wrong answers.
                self.plugin_state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .in_transaction = true;
                self.status.transaction = iso.map(|level| isolation_label(level).to_owned());
                self.status.message = match iso {
                    Some(level) => format!("transaction started ({})", isolation_label(level)),
                    None => "transaction started".into(),
                };
            }
            Err(error) => {
                self.status.message = format!("begin failed: {error}");
            }
        }
    }

    pub(super) fn commit_transaction(&mut self) {
        self.end_transaction(true);
    }

    pub(super) fn rollback_transaction(&mut self) {
        self.end_transaction(false);
    }

    /// Finish an open transaction. `commit == true` invokes `commit()`,
    /// otherwise `rollback()`. Either way the pinned connection is
    /// returned to the pool.
    pub(super) fn end_transaction(&mut self, commit: bool) {
        if self.running {
            self.status.message = "a query is already running".into();
            return;
        }
        let Some(session) = self.session.as_mut() else {
            self.status.message = "no active connection".into();
            return;
        };
        // Peek at the Arc without taking it: if another holder is still
        // around (a run worker, a paused stream) the commit cannot
        // succeed and we must leave `session.transaction` in place so
        // the host's view stays in sync with the server. Previously we
        // `take()`'d unconditionally and emitted "connection still in
        // use", which silently de-synced the session (Round 2 fix).
        let Some(txn) = session.transaction.as_ref() else {
            self.status.message = "no open transaction".into();
            return;
        };
        if Arc::strong_count(&txn.conn) > 1 {
            self.status.message = if commit {
                "commit failed: transaction connection still in use".into()
            } else {
                "rollback failed: transaction connection still in use".into()
            };
            return;
        }
        // Safe to take the transaction now — the only Arc clone is the
        // one we're about to consume.
        let txn = session.transaction.take().expect("checked above");
        let conn_arc = txn.conn;
        let outcome = tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(async move {
                let mutex = Arc::try_unwrap(conn_arc).map_err(|_| {
                    narwhal_core::Error::Connection(
                        "transaction connection raced after strong-count check".into(),
                    )
                })?;
                let mut conn = mutex.into_inner();
                if commit {
                    conn.commit().await?;
                } else {
                    conn.rollback().await?;
                }
                Ok::<(), narwhal_core::Error>(())
            })
        });
        // Clear the plugin-side flag so subsequent `sql_run` calls work
        // against the pool again. The connection itself is now back in
        // the pool (PooledConnection::drop returned it), regardless of
        // whether the commit/rollback round-trip succeeded.
        self.plugin_state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .in_transaction = false;
        self.status.transaction = None;
        match outcome {
            Ok(()) => {
                self.status.message = if commit {
                    "transaction committed".into()
                } else {
                    "transaction rolled back".into()
                };
            }
            Err(error) => {
                self.status.message = if commit {
                    format!("commit failed: {error}")
                } else {
                    format!("rollback failed: {error}")
                };
            }
        }
    }

    pub(super) fn savepoint(&mut self, name: &str) {
        self.with_txn_conn(
            |conn, name| Box::pin(async move { conn.savepoint(name).await }),
            name,
            |session, name| {
                if let Some(txn) = session.transaction.as_mut() {
                    txn.savepoints.push(name.to_owned());
                }
            },
            |name| format!("savepoint '{name}' established"),
            |name, error| format!("savepoint '{name}' failed: {error}"),
        );
    }

    pub(super) fn release_savepoint(&mut self, name: &str) {
        self.with_txn_conn(
            |conn, name| Box::pin(async move { conn.release_savepoint(name).await }),
            name,
            |session, name| {
                if let Some(txn) = session.transaction.as_mut() {
                    if let Some(pos) = txn.savepoints.iter().position(|s| s == name) {
                        txn.savepoints.truncate(pos);
                    }
                }
            },
            |name| format!("savepoint '{name}' released"),
            |name, error| format!("release '{name}' failed: {error}"),
        );
    }

    pub(super) fn rollback_to_savepoint(&mut self, name: &str) {
        self.with_txn_conn(
            |conn, name| Box::pin(async move { conn.rollback_to_savepoint(name).await }),
            name,
            |session, name| {
                if let Some(txn) = session.transaction.as_mut() {
                    if let Some(pos) = txn.savepoints.iter().position(|s| s == name) {
                        // Everything after the savepoint is unwound.
                        txn.savepoints.truncate(pos + 1);
                    }
                }
            },
            |name| format!("rolled back to savepoint '{name}'"),
            |name, error| format!("rollback-to '{name}' failed: {error}"),
        );
    }

    /// Lock the pinned transaction connection and run `op` on it. Used by
    /// `:savepoint`, `:release` and `:rollback-to` which all need the same
    /// guarding boilerplate. Statement execution (`:run`/`:run-all`) goes
    /// through `dispatch_batch` instead since that path streams updates
    /// back through `RunUpdate`.
    fn with_txn_conn<F, S, OkF, ErrF>(
        &mut self,
        op: F,
        name: &str,
        on_success: S,
        ok_msg: OkF,
        err_msg: ErrF,
    ) where
        F: for<'a> FnOnce(
            &'a mut dyn narwhal_core::Connection,
            &'a str,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = narwhal_core::Result<()>> + Send + 'a>,
        >,
        S: FnOnce(&mut Session, &str),
        OkF: FnOnce(&str) -> String,
        ErrF: FnOnce(&str, &narwhal_core::Error) -> String,
    {
        if self.running {
            self.status.message = "a query is already running".into();
            return;
        }
        let Some(session) = self.session.as_mut() else {
            self.status.message = "no active connection".into();
            return;
        };
        let Some(txn) = session.transaction.as_ref() else {
            self.status.message = "no open transaction".into();
            return;
        };
        let conn_arc = txn.conn.clone();
        let result = tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(async move {
                let mut guard = conn_arc.lock().await;
                op(&mut **guard, name).await
            })
        });
        match result {
            Ok(()) => {
                on_success(session, name);
                self.status.message = ok_msg(name);
            }
            Err(error) => {
                self.status.message = err_msg(name, &error);
            }
        }
    }
}
