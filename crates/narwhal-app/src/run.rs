use std::sync::Arc;
use std::time::Instant;

use narwhal_core::{CancelHandle, ColumnHeader, Connection, Row, Value};
use narwhal_history::{HistoryEntry, Journal};
use narwhal_pool::{Pool, PooledConnection};
use tokio::sync::{mpsc, Mutex};
use tracing::warn;
use uuid::Uuid;

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
    pub connection_id: Uuid,
    pub connection_name: String,
    pub driver: String,
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
pub type ActiveCancel = Arc<Mutex<Option<Box<dyn CancelHandle>>>>;

const STREAM_BATCH: usize = 64;

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
            fn conn(&mut self) -> &mut dyn Connection {
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
                RunMode::Stream => run_stream(holder.conn(), sql, params, &tx).await,
            };

            *cancel_slot.lock().await = None;

            record_history(&ctx, sql, &outcome).await;

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

async fn run_execute(
    conn: &mut dyn narwhal_core::Connection,
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
    conn: &mut dyn narwhal_core::Connection,
    sql: &str,
    params: &[Value],
    tx: &mpsc::Sender<RunUpdate>,
) -> StatementOutcome {
    let started = Instant::now();
    let mut stream = match conn.stream(sql, params).await {
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

    let mut total_rows = 0usize;
    let mut batch: Vec<Row> = Vec::with_capacity(STREAM_BATCH);
    let mut terminal_error: Option<narwhal_core::Error> = None;

    loop {
        match stream.next_row().await {
            Ok(Some(row)) => {
                batch.push(row);
                total_rows += 1;
                if batch.len() >= STREAM_BATCH {
                    let chunk = std::mem::replace(&mut batch, Vec::with_capacity(STREAM_BATCH));
                    let _ = tx.send(RunUpdate::RowsAppended { rows: chunk }).await;
                }
            }
            Ok(None) => break,
            Err(error) => {
                terminal_error = Some(error);
                break;
            }
        }
    }

    if !batch.is_empty() {
        let _ = tx.send(RunUpdate::RowsAppended { rows: batch }).await;
    }

    let elapsed_ms = started.elapsed().as_millis() as u64;
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

#[cfg(test)]
mod tests {
    use super::is_ddl_statement;

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
