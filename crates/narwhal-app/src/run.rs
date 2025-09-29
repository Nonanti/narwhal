use std::sync::Arc;
use std::time::Instant;

use narwhal_core::{CancelHandle, ColumnHeader, Row};
use narwhal_history::{HistoryEntry, Journal};
use narwhal_pool::Pool;
use tokio::sync::{mpsc, Mutex};
use tracing::warn;
use uuid::Uuid;

/// How the worker should execute a statement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunMode {
    /// Materialise the entire result on the connection and deliver it as a
    /// single chunk. Drivers report `rows_affected` for non-SELECT statements.
    Execute,
    /// Stream rows back as the engine produces them. Suitable for large or
    /// open-ended result sets.
    Stream,
}

/// Batch of statements queued for execution against a single connection.
#[derive(Debug, Clone)]
pub struct RunRequest {
    pub statements: Vec<String>,
    pub mode: RunMode,
}

/// Context shared across dispatches.
#[derive(Clone)]
pub struct RunContext {
    pub pool: Pool,
    pub history: Option<Arc<Journal>>,
    pub connection_id: Uuid,
    pub connection_name: String,
    pub driver: String,
}

/// Incremental updates produced by the worker.
///
/// The UI consumes these to build a [`crate::app::ResultState`] without
/// stalling the event loop.
#[derive(Debug)]
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
    Failed { error: String, elapsed_ms: u64 },
    /// The whole batch has terminated.
    AllDone { successes: usize, failures: usize },
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
                })
                .await;
            return;
        }

        let mut conn = match ctx.pool.acquire().await {
            Ok(c) => c,
            Err(error) => {
                let _ = tx
                    .send(RunUpdate::Failed {
                        error: error.to_string(),
                        elapsed_ms: 0,
                    })
                    .await;
                let _ = tx
                    .send(RunUpdate::AllDone {
                        successes: 0,
                        failures: total,
                    })
                    .await;
                return;
            }
        };

        let mut successes = 0;
        let mut failures = 0;

        for (i, sql) in request.statements.iter().enumerate() {
            let _ = tx
                .send(RunUpdate::StatementStarted {
                    index: i + 1,
                    total,
                    sql: sql.clone(),
                })
                .await;

            if let Some(handle) = conn.cancel_handle() {
                *cancel_slot.lock().await = Some(handle);
            }

            let outcome = match request.mode {
                RunMode::Execute => run_execute(&mut *conn, sql, &tx).await,
                RunMode::Stream => run_stream(&mut *conn, sql, &tx).await,
            };

            *cancel_slot.lock().await = None;

            record_history(&ctx, sql, &outcome).await;

            match &outcome {
                StatementOutcome::Ok { .. } => successes += 1,
                StatementOutcome::Err { .. } => {
                    failures += 1;
                    break;
                }
            }
        }

        let _ = tx
            .send(RunUpdate::AllDone {
                successes,
                failures,
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
    tx: &mpsc::Sender<RunUpdate>,
) -> StatementOutcome {
    let started = Instant::now();
    match conn.execute(sql, &[]).await {
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
            let _ = tx
                .send(RunUpdate::Failed {
                    error: error.to_string(),
                    elapsed_ms,
                })
                .await;
            StatementOutcome::Err { error, elapsed_ms }
        }
    }
}

async fn run_stream(
    conn: &mut dyn narwhal_core::Connection,
    sql: &str,
    tx: &mpsc::Sender<RunUpdate>,
) -> StatementOutcome {
    let started = Instant::now();
    let mut stream = match conn.stream(sql, &[]).await {
        Ok(s) => s,
        Err(error) => {
            let elapsed_ms = started.elapsed().as_millis() as u64;
            let _ = tx
                .send(RunUpdate::Failed {
                    error: error.to_string(),
                    elapsed_ms,
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
        let _ = tx
            .send(RunUpdate::Failed {
                error: error.to_string(),
                elapsed_ms,
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
