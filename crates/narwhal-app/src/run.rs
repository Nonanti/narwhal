use std::sync::Arc;
use std::time::Instant;

use narwhal_core::{CancelHandle, QueryResult};
use narwhal_history::{HistoryEntry, Journal};
use narwhal_pool::Pool;
use tokio::sync::{mpsc, Mutex};
use tracing::warn;
use uuid::Uuid;

/// Outcome delivered back to the UI thread once a query completes.
#[derive(Debug)]
pub struct RunResult {
    pub sql: String,
    pub outcome: Result<QueryResult, narwhal_core::Error>,
}

/// Context shared across run dispatches.
#[derive(Clone)]
pub struct RunContext {
    pub pool: Pool,
    pub history: Option<Arc<Journal>>,
    pub connection_id: Uuid,
    pub connection_name: String,
    pub driver: String,
}

/// Handle to the in-flight query. Holding this allows the UI to issue a
/// best-effort cancellation. Dropping it does not cancel the query.
pub type ActiveCancel = Arc<Mutex<Option<Box<dyn CancelHandle>>>>;

pub fn spawn_query(
    ctx: RunContext,
    sql: String,
    cancel_slot: ActiveCancel,
    completion: mpsc::Sender<RunResult>,
) {
    tokio::spawn(async move {
        let started = Instant::now();
        let acquire = ctx.pool.acquire().await;
        let outcome = match acquire {
            Err(error) => Err(narwhal_core::Error::Connection(error.to_string())),
            Ok(mut conn) => {
                if let Some(handle) = conn.cancel_handle() {
                    let mut slot = cancel_slot.lock().await;
                    *slot = Some(handle);
                }
                let result = conn.execute(&sql, &[]).await;
                let mut slot = cancel_slot.lock().await;
                *slot = None;
                result
            }
        };
        let elapsed = started.elapsed().as_millis() as u64;
        record_history(&ctx, &sql, elapsed, &outcome).await;

        let _ = completion.send(RunResult { sql, outcome }).await;
    });
}

async fn record_history(
    ctx: &RunContext,
    sql: &str,
    elapsed_ms: u64,
    outcome: &Result<QueryResult, narwhal_core::Error>,
) {
    let Some(journal) = ctx.history.as_ref() else {
        return;
    };
    let mut entry = HistoryEntry::success(sql.to_owned())
        .with_connection(ctx.connection_id, ctx.connection_name.clone())
        .with_driver(ctx.driver.clone())
        .with_timing(elapsed_ms);
    match outcome {
        Ok(result) => {
            if let Some(affected) = result.rows_affected {
                entry = entry.with_rows_affected(affected);
            }
            entry = entry.with_rows_returned(result.rows.len() as u64);
        }
        Err(narwhal_core::Error::Cancelled) => {
            entry = entry.with_cancellation();
        }
        Err(error) => {
            entry = entry.with_failure(error.to_string());
        }
    }
    if let Err(error) = journal.append(&entry).await {
        warn!(target: "narwhal::run", error = %error, "history append failed");
    }
}
