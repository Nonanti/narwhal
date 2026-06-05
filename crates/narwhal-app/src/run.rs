use std::sync::Arc;
use std::time::{Duration, Instant};

use narwhal_audit::{AuditEvent, AuditService};
use narwhal_core::{ColumnHeader, DynCancelHandle, DynConnection, Row, Value};
use narwhal_history::{HistoryEntry, Journal};
use narwhal_pool::{Pool, PooledConnection};
use tokio::sync::{Mutex, mpsc};
use tracing::warn;
use uuid::Uuid;

/// Streaming-result tuning, sourced from `settings.run` and passed
/// into the worker. Local copy so the worker does not have to depend
/// on `narwhal-config` directly — the binary translates from
/// [`narwhal_config::RunSettings`] at the call site.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct StreamTuning {
    /// Maximum batch size before a flush, in rows.
    pub batch_size: usize,
    /// Time window (ms) before a partially-filled batch is flushed.
    /// `0` disables the time-based flush.
    pub flush_ms: u64,
}

impl Default for StreamTuning {
    /// Pre-T1-T4-A defaults: 64-row batch, 50 ms flush window.
    fn default() -> Self {
        Self {
            batch_size: 64,
            flush_ms: 50,
        }
    }
}

impl StreamTuning {
    /// Build a tuning struct, defending against the pathological
    /// `batch_size = 0` config (would loop forever).
    #[must_use]
    pub const fn new(batch_size: usize, flush_ms: u64) -> Self {
        let batch_size = if batch_size == 0 { 1 } else { batch_size };
        Self {
            batch_size,
            flush_ms,
        }
    }
}

/// Check whether a SQL statement is a DDL statement by inspecting its
/// first token. Matches CREATE, DROP, ALTER, TRUNCATE, RENAME
/// (case-insensitive). Leading SQL comments (`-- ...\n` and `/* ... */`)
/// are skipped first so a comment-prefixed migration still triggers the
/// schema-refresh side-effect.
pub fn is_ddl_statement(sql: &str) -> bool {
    let head = strip_leading_comments(sql)
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_ascii_uppercase();
    matches!(
        head.as_str(),
        "CREATE" | "DROP" | "ALTER" | "TRUNCATE" | "RENAME"
    )
}

/// Trim leading whitespace and SQL comments from `sql`. Stops as soon
/// as a non-comment token begins. Handles nested block comments
/// conservatively (only the outermost `*/` ends the comment).
fn strip_leading_comments(sql: &str) -> &str {
    let mut s = sql;
    loop {
        let trimmed = s.trim_start();
        if let Some(rest) = trimmed.strip_prefix("--") {
            // Skip to end of line.
            let end = rest.find('\n').map_or(rest.len(), |i| i + 1);
            s = &rest[end..];
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("/*") {
            // Skip to the next `*/`.
            if let Some(end) = rest.find("*/") {
                s = &rest[end + 2..];
                continue;
            }
            // Unterminated block comment — there's no statement to
            // classify.
            return "";
        }
        return trimmed;
    }
}

/// How the worker should execute a statement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum RunMode {
    /// Materialise the entire result on the connection and deliver it as a
    /// single chunk. Drivers report `rows_affected` for non-SELECT statements.
    Execute,
    /// Stream rows back as the engine produces them. Suitable for large or
    /// open-ended result sets.
    Stream,
}

/// Batch of statements queued for execution against a single connection.
///
/// `params_per_statement[i]` carries the bound parameters for
/// `statements[i]`; when empty (the common interactive case) the worker
/// invokes `execute(sql, &[])`. Internal callsites that need parametric
/// dispatch (e.g. foreign-key navigation, where the cell value must not
/// be interpolated as SQL) build the request via
/// [`RunRequest::with_params`].
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct RunRequest {
    pub statements: Vec<String>,
    pub mode: RunMode,
    /// Bound parameters per statement. Length 0 means “no bindings for any
    /// statement”; otherwise `params_per_statement.len() == statements.len()`.
    pub params_per_statement: Vec<Vec<Value>>,
}

impl RunRequest {
    /// Plain batch with no bound parameters. The interactive editor path.
    #[must_use]
    pub const fn new(statements: Vec<String>, mode: RunMode) -> Self {
        Self {
            statements,
            mode,
            params_per_statement: Vec::new(),
        }
    }

    /// Parametric batch. `items[i].0` is the SQL, `items[i].1` is the
    /// bound parameter vector for that statement.
    #[must_use]
    pub fn with_params(items: Vec<(String, Vec<Value>)>, mode: RunMode) -> Self {
        let (statements, params_per_statement) = items.into_iter().unzip();
        Self {
            statements,
            mode,
            params_per_statement,
        }
    }

    /// Bound parameters for `index`, or an empty slice when none.
    #[must_use]
    pub fn params_for(&self, index: usize) -> &[Value] {
        self.params_per_statement
            .get(index)
            .map_or(&[][..], Vec::as_slice)
    }
}

/// Where the worker should source the connection from.
#[derive(Clone)]
#[non_exhaustive]
pub enum RunTarget {
    /// Acquire a fresh connection from the pool and return it on completion.
    Pool(Pool),
    /// Reuse a connection pinned to an open transaction. The worker locks
    /// the mutex for the duration of the batch.
    Pinned(Arc<Mutex<PooledConnection>>),
}

/// Context shared across dispatches.
#[derive(Clone)]
pub struct RunContext {
    pub target: RunTarget,
    pub history: Option<Arc<Journal>>,
    /// T2-T2-D: optional audit log service. Set from
    /// `SessionState::audit_service`. When present, every dispatched
    /// statement emits an [`AuditEvent::Query`] alongside the history
    /// journal write.
    pub audit: Option<Arc<AuditService>>,
    /// T2-T2-D: session correlation id for [`AuditEvent::Query`].
    /// Stable for the lifetime of the active connection so a SIEM can
    /// stitch queries back to the originating session.
    pub audit_session_id: Uuid,
    pub connection_id: Uuid,
    pub connection_name: String,
    pub driver: String,
    /// Streaming-result tuning. Caller passes the live value from
    /// `SessionState::stream_tuning`; tests can pass
    /// [`StreamTuning::default`] directly.
    pub stream_tuning: StreamTuning,
}

/// Incremental updates produced by the worker.
///
/// The UI consumes these to build a [`crate::core::ResultState`] without
/// stalling the event loop.
#[derive(Debug)]
#[non_exhaustive]
pub enum RunUpdate {
    /// A new statement is about to run. `index` and `total` are 1-based.
    StatementStarted {
        index: usize,
        total: usize,
        sql: String,
    },
    /// Column headers became available. Always emitted before any
    /// [`RunUpdate::RowsAppended`] for the current statement.
    HeaderReady { columns: Vec<ColumnHeader> },
    /// A batch of rows for the currently running statement.
    RowsAppended { rows: Vec<Row> },
    /// The current statement finished successfully.
    StatementFinished {
        elapsed_ms: u64,
        rows_returned: usize,
        rows_affected: Option<u64>,
        streamed: bool,
    },
    /// The current statement failed; the batch is aborted.
    /// `cancelled` is `true` when the failure was caused by a user-initiated
    /// cancellation (e.g. Ctrl+C), allowing the UI to distinguish cancellation
    /// from a genuine query error.
    Failed {
        error: String,
        elapsed_ms: u64,
        cancelled: bool,
    },
    /// The whole batch has terminated.
    AllDone {
        successes: usize,
        failures: usize,
        /// Whether any successful statement in the batch was DDL.
        ddl: bool,
    },
    /// A debounced schema refresh is due. Sent by the debounce timer
    /// task, not by the run worker. `session_id` is the connection id
    /// that owned the DDL batch — the handler must discard the
    /// notification if the user has since switched to a different
    /// session (bug C5).
    SchemaRefresh { session_id: Uuid },
}

/// Handle to the in-flight statement.
pub type ActiveCancel = Arc<Mutex<Option<Box<dyn DynCancelHandle>>>>;

pub fn spawn_run(
    ctx: RunContext,
    request: RunRequest,
    cancel_slot: ActiveCancel,
    tx: mpsc::Sender<RunUpdate>,
) {
    tokio::spawn(async move {
        let total = request.statements.len();
        if total == 0 {
            let _ = tx
                .send(RunUpdate::AllDone {
                    successes: 0,
                    failures: 0,
                    ddl: false,
                })
                .await;
            return;
        }

        // Source the connection. Pool target -> a fresh PooledConnection;
        // Pinned target -> a tokio OwnedMutexGuard locked for the whole
        // batch so nothing else can interleave statements onto the same
        // transaction.
        enum Holder {
            Owned(PooledConnection),
            Pinned(tokio::sync::OwnedMutexGuard<PooledConnection>),
        }
        impl Holder {
            fn conn(&mut self) -> &mut dyn DynConnection {
                // The match bindings are `&mut PooledConnection` and
                // `&mut OwnedMutexGuard<PooledConnection>`, so we need an
                // extra deref step in each arm to reach `dyn Connection`.
                match self {
                    Self::Owned(c) => &mut **c,
                    Self::Pinned(g) => &mut ***g,
                }
            }
        }
        let mut holder = match &ctx.target {
            RunTarget::Pool(pool) => match pool.acquire().await {
                Ok(c) => Holder::Owned(c),
                Err(error) => {
                    let _ = tx
                        .send(RunUpdate::Failed {
                            error: error.to_string(),
                            elapsed_ms: 0,
                            cancelled: false,
                        })
                        .await;
                    let _ = tx
                        .send(RunUpdate::AllDone {
                            successes: 0,
                            failures: total,
                            ddl: false,
                        })
                        .await;
                    return;
                }
            },
            RunTarget::Pinned(handle) => Holder::Pinned(Arc::clone(handle).lock_owned().await),
        };

        let mut successes = 0;
        let mut failures = 0;
        let mut ddl = false;

        for (i, sql) in request.statements.iter().enumerate() {
            let _ = tx
                .send(RunUpdate::StatementStarted {
                    index: i + 1,
                    total,
                    sql: sql.clone(),
                })
                .await;

            if let Some(handle) = holder.conn().cancel_handle() {
                *cancel_slot.lock().await = Some(handle);
            }

            let params = request.params_for(i);
            let outcome = match request.mode {
                RunMode::Execute => run_execute(holder.conn(), sql, params, &tx).await,
                RunMode::Stream => {
                    run_stream(holder.conn(), sql, params, ctx.stream_tuning, &tx).await
                }
            };

            *cancel_slot.lock().await = None;

            record_history(&ctx, sql, &outcome).await;
            emit_audit_query(&ctx, sql, &outcome).await;

            match &outcome {
                StatementOutcome::Ok { .. } => {
                    successes += 1;
                    if is_ddl_statement(sql) {
                        ddl = true;
                    }
                }
                StatementOutcome::Err { .. } => {
                    failures += 1;
                    break;
                }
            }
        }
        drop(holder);

        let _ = tx
            .send(RunUpdate::AllDone {
                successes,
                failures,
                ddl,
            })
            .await;
    });
}

enum StatementOutcome {
    Ok {
        elapsed_ms: u64,
        rows_returned: usize,
        rows_affected: Option<u64>,
    },
    Err {
        error: narwhal_core::Error,
        elapsed_ms: u64,
    },
}

/// Internal control-flow tag for the streaming loop: every iteration
/// of `run_stream` boils down to "got a row", "engine ended", "engine
/// errored", or "timer fired so we flushed a partial batch and we
/// should keep going". A dedicated enum keeps the `match` in the
/// hot loop exhaustive and easier to extend.
enum NextRowOutcome {
    Row(Row),
    End,
    Err(narwhal_core::Error),
    /// The time-window timer fired before a row arrived. The loop
    /// has already flushed any pending batch; iterate.
    Continue,
}

async fn run_execute(
    conn: &mut dyn narwhal_core::DynConnection,
    sql: &str,
    params: &[Value],
    tx: &mpsc::Sender<RunUpdate>,
) -> StatementOutcome {
    let started = Instant::now();
    match conn.execute(sql, params).await {
        Ok(qr) => {
            let elapsed_ms = started.elapsed().as_millis() as u64;
            let _ = tx
                .send(RunUpdate::HeaderReady {
                    columns: qr.columns.clone(),
                })
                .await;
            let rows_returned = qr.rows.len();
            if !qr.rows.is_empty() {
                let _ = tx.send(RunUpdate::RowsAppended { rows: qr.rows }).await;
            }
            let _ = tx
                .send(RunUpdate::StatementFinished {
                    elapsed_ms,
                    rows_returned,
                    rows_affected: qr.rows_affected,
                    streamed: false,
                })
                .await;
            StatementOutcome::Ok {
                elapsed_ms,
                rows_returned,
                rows_affected: qr.rows_affected,
            }
        }
        Err(error) => {
            let elapsed_ms = started.elapsed().as_millis() as u64;
            let cancelled = matches!(&error, narwhal_core::Error::Cancelled);
            let _ = tx
                .send(RunUpdate::Failed {
                    error: error.to_string(),
                    elapsed_ms,
                    cancelled,
                })
                .await;
            StatementOutcome::Err { error, elapsed_ms }
        }
    }
}

async fn run_stream(
    conn: &mut dyn narwhal_core::DynConnection,
    sql: &str,
    params: &[Value],
    tuning: StreamTuning,
    tx: &mpsc::Sender<RunUpdate>,
) -> StatementOutcome {
    let started = Instant::now();
    let mut stream = match conn.query(sql, params).await {
        Ok(s) => s,
        Err(error) => {
            let elapsed_ms = started.elapsed().as_millis() as u64;
            let cancelled = matches!(&error, narwhal_core::Error::Cancelled);
            let _ = tx
                .send(RunUpdate::Failed {
                    error: error.to_string(),
                    elapsed_ms,
                    cancelled,
                })
                .await;
            return StatementOutcome::Err { error, elapsed_ms };
        }
    };

    let _ = tx
        .send(RunUpdate::HeaderReady {
            columns: stream.columns().to_vec(),
        })
        .await;

    let mut batch: Vec<Row> = Vec::with_capacity(tuning.batch_size.max(1));
    let mut terminal_error: Option<narwhal_core::Error> = None;
    // T1-T4-A: time-window flush. `last_flush` is reset every time
    // we send a `RowsAppended` so a slow trickle still surfaces
    // rows in the UI within `flush_ms`. When `flush_ms == 0` the
    // time-based flush is disabled and we revert to pure size
    // batching (v1.x behaviour).
    let mut last_flush = Instant::now();
    let flush_window = if tuning.flush_ms == 0 {
        None
    } else {
        Some(Duration::from_millis(tuning.flush_ms))
    };
    // Defensive clamp: `batch_size = 0` would loop-flush every row,
    // saturating the UI channel. `StreamTuning::new` already
    // clamps to 1, but the field is `pub` so a runtime mutation
    // could bypass that — keep a local floor so the worker is
    // bullet-proof regardless.
    let batch_size = tuning.batch_size.max(1);

    loop {
        // Drive the next row with an optional time-based deadline so
        // a partial batch can flush even when the driver is idle.
        //
        // Cancellation safety: when the timeout fires the inner
        // future is dropped before resolving. We rely on the inner
        // row-yielding future being cancellation-safe — verified
        // for the sqlite / duckdb path (the row crosses through a
        // `tokio::sync::mpsc::Receiver::recv`, which is
        // cancellation-safe by tokio's contract). The sqlx-backed
        // (PG / MySQL) and ClickHouse paths have not been audited
        // line-by-line; if a driver author finds the cancellation
        // contract is not upheld for their backend, they should
        // surface that as a [`Capabilities`] flag and we can branch
        // the worker on it. Until then the default tuning's 50 ms
        // window keeps the timeout-cancel rate low enough that the
        // worst-case behaviour is one mid-protocol drop per slow
        // query, which sqlx handles by closing the connection —
        // costly but not corrupting.
        let next_row_outcome = if let Some(window) = flush_window {
            let elapsed_since_flush = last_flush.elapsed();
            // If the window already elapsed, flush any pending
            // batch (or just reset the timer if nothing's pending)
            // before computing the timeout. This is the fix for the
            // "empty batch / expired window" busy-loop: without the
            // unconditional reset, the next iteration would see
            // `remaining == 0` again and spin.
            if elapsed_since_flush >= window {
                if !batch.is_empty() {
                    let chunk = std::mem::replace(&mut batch, Vec::with_capacity(batch_size));
                    let _ = tx.send(RunUpdate::RowsAppended { rows: chunk }).await;
                }
                last_flush = Instant::now();
            }
            // Always wait at least 1 ms on the next_row future
            // — a zero-duration timeout would resolve before the
            // driver gets a chance to schedule, manifesting as a
            // spin.
            let remaining = window
                .saturating_sub(last_flush.elapsed())
                .max(Duration::from_millis(1));
            match tokio::time::timeout(remaining, stream.next_row()).await {
                Ok(Some(Ok(row))) => NextRowOutcome::Row(row),
                Ok(Some(Err(err))) => NextRowOutcome::Err(err),
                Ok(None) => NextRowOutcome::End,
                Err(_elapsed) => {
                    // Timer fired before a row arrived. Flush
                    // anything we already have and reset.
                    if !batch.is_empty() {
                        let chunk = std::mem::replace(&mut batch, Vec::with_capacity(batch_size));
                        let _ = tx.send(RunUpdate::RowsAppended { rows: chunk }).await;
                    }
                    last_flush = Instant::now();
                    NextRowOutcome::Continue
                }
            }
        } else {
            match stream.next_row().await {
                Some(Ok(row)) => NextRowOutcome::Row(row),
                Some(Err(err)) => NextRowOutcome::Err(err),
                None => NextRowOutcome::End,
            }
        };

        match next_row_outcome {
            NextRowOutcome::Row(row) => {
                batch.push(row);
                if batch.len() >= batch_size {
                    let chunk = std::mem::replace(&mut batch, Vec::with_capacity(batch_size));
                    let _ = tx.send(RunUpdate::RowsAppended { rows: chunk }).await;
                    last_flush = Instant::now();
                }
            }
            NextRowOutcome::End => break,
            NextRowOutcome::Err(error) => {
                terminal_error = Some(error);
                break;
            }
            NextRowOutcome::Continue => {}
        }
    }

    if !batch.is_empty() {
        let _ = tx.send(RunUpdate::RowsAppended { rows: batch }).await;
    }

    let elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    // Single source of truth for the row count: ask the stream
    // (review fixup M4). Avoids the previous `total_rows` shadow
    // counter that could drift if the loop body ever grew a
    // "skip this row" branch.
    let total_rows = stream.rows_yielded();
    if let Err(error) = stream.close().await {
        warn!(target: "narwhal::run", error = %error, "stream close failed");
    }

    if let Some(error) = terminal_error {
        let cancelled = matches!(&error, narwhal_core::Error::Cancelled);
        let _ = tx
            .send(RunUpdate::Failed {
                error: error.to_string(),
                elapsed_ms,
                cancelled,
            })
            .await;
        StatementOutcome::Err { error, elapsed_ms }
    } else {
        let _ = tx
            .send(RunUpdate::StatementFinished {
                elapsed_ms,
                rows_returned: total_rows,
                rows_affected: None,
                streamed: true,
            })
            .await;
        StatementOutcome::Ok {
            elapsed_ms,
            rows_returned: total_rows,
            rows_affected: None,
        }
    }
}

async fn record_history(ctx: &RunContext, sql: &str, outcome: &StatementOutcome) {
    let Some(journal) = ctx.history.as_ref() else {
        return;
    };
    let mut entry = HistoryEntry::success(sql.to_owned())
        .with_connection(ctx.connection_id, ctx.connection_name.clone())
        .with_driver(ctx.driver.clone());
    match outcome {
        StatementOutcome::Ok {
            elapsed_ms,
            rows_returned,
            rows_affected,
        } => {
            entry = entry.with_timing(*elapsed_ms);
            if let Some(a) = rows_affected {
                entry = entry.with_rows_affected(*a);
            }
            entry = entry.with_rows_returned(*rows_returned as u64);
        }
        StatementOutcome::Err { error, elapsed_ms } => {
            entry = entry.with_timing(*elapsed_ms);
            entry = match error {
                narwhal_core::Error::Cancelled => entry.with_cancellation(),
                _ => entry.with_failure(error.to_string()),
            };
        }
    }
    if let Err(error) = journal.append(&entry).await {
        warn!(target: "narwhal::run", error = %error, "history append failed");
    }
}

/// T2-T2-D: project the just-executed statement into the audit log.
///
/// Mirrors [`record_history`] structurally so the two sinks stay in
/// lock-step: every statement that lands in `history.jsonl` also
/// lands in the configured audit sinks (when audit is enabled).
///
/// Bind parameters are not currently threaded through `RunContext`
/// — the run worker collapses them into the SQL text upstream of
/// this site. The empty `params` vector is therefore intentional;
/// the field is kept on the wire-format so future parameterised
/// dispatch paths can fill it without a schema bump.
async fn emit_audit_query(ctx: &RunContext, sql: &str, outcome: &StatementOutcome) {
    let Some(audit) = ctx.audit.as_ref() else {
        return;
    };
    let event = match outcome {
        StatementOutcome::Ok {
            elapsed_ms,
            rows_returned,
            rows_affected,
        } => AuditEvent::Query {
            session_id: ctx.audit_session_id,
            sql: sql.to_owned(),
            params: Vec::new(),
            rows: rows_affected.or(Some(*rows_returned as u64)),
            elapsed_ms: *elapsed_ms,
            succeeded: true,
            error: None,
        },
        StatementOutcome::Err { error, elapsed_ms } => AuditEvent::Query {
            session_id: ctx.audit_session_id,
            sql: sql.to_owned(),
            params: Vec::new(),
            rows: None,
            elapsed_ms: *elapsed_ms,
            succeeded: false,
            error: Some(error.to_string()),
        },
    };
    audit.emit(event).await;
}

#[cfg(test)]
mod tests {
    use super::{StreamTuning, is_ddl_statement};

    /// T1-T4-A: `StreamTuning::new` must not let `batch_size = 0`
    /// escape — a zero-sized batch would livelock the worker loop.
    #[test]
    fn stream_tuning_zero_batch_clamped_to_one() {
        let t = StreamTuning::new(0, 50);
        assert_eq!(t.batch_size, 1);
        assert_eq!(t.flush_ms, 50);
    }

    #[test]
    fn stream_tuning_passthrough_when_positive() {
        let t = StreamTuning::new(128, 25);
        assert_eq!(t.batch_size, 128);
        assert_eq!(t.flush_ms, 25);
    }

    #[test]
    fn stream_tuning_default_matches_v1() {
        let t = StreamTuning::default();
        assert_eq!(t.batch_size, 64);
        assert_eq!(t.flush_ms, 50);
    }

    #[test]
    fn ddl_classifier_matches_keywords() {
        assert!(is_ddl_statement("CREATE TABLE t (id INT)"));
        assert!(is_ddl_statement("DROP TABLE t"));
        assert!(is_ddl_statement("ALTER TABLE t ADD col INT"));
        assert!(is_ddl_statement("TRUNCATE TABLE t"));
        assert!(is_ddl_statement("RENAME TABLE t TO u"));
    }

    #[test]
    fn ddl_classifier_case_insensitive() {
        assert!(is_ddl_statement("create table t (id int)"));
        assert!(is_ddl_statement("drop table t"));
        assert!(is_ddl_statement("CrEaTe TABLE t (id INT)"));
    }

    #[test]
    fn ddl_classifier_leading_whitespace() {
        assert!(is_ddl_statement("   CREATE TABLE t (id INT)"));
        assert!(is_ddl_statement("\n\tDROP TABLE t"));
    }

    /// Round 1 bugfix: leading SQL comments used to break the
    /// classifier so a comment-prefixed migration would not trigger
    /// the post-DDL schema-refresh side-effect.
    #[test]
    fn ddl_classifier_skips_leading_comments() {
        assert!(is_ddl_statement(
            "-- migration 0001\nCREATE TABLE t (id INT)"
        ));
        assert!(is_ddl_statement("/* block */ DROP TABLE t"));
        assert!(is_ddl_statement(
            "-- one\n-- two\n /* three */ ALTER TABLE t ADD x INT"
        ));
        // Unterminated block comment: defensively classify as non-DDL.
        assert!(!is_ddl_statement("/* open"));
    }

    #[test]
    fn ddl_classifier_rejects_non_ddl() {
        assert!(!is_ddl_statement("SELECT * FROM t"));
        assert!(!is_ddl_statement("INSERT INTO t VALUES (1)"));
        assert!(!is_ddl_statement("UPDATE t SET x = 1"));
        assert!(!is_ddl_statement("DELETE FROM t"));
        assert!(!is_ddl_statement(""));
        assert!(!is_ddl_statement("   "));
    }
}
