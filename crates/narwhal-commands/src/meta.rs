//! Background metadata operations channel.
//!
//! Long-running metadata queries (`dump_schema all`, `refresh_schemas`,
//! `open_history`) used to block the UI via `block_in_place + block_on`.
//! This module provides a request/response channel modelled on
//! `RunUpdate` (in the host crate) so these operations can run on a tokio
//! worker without stalling the event loop (H11).
//!
//! The channel is intentionally separate from the run channel so that
//! a slow metadata operation does not interfere with statement
//! execution state (`self.running`, cancel handles, etc.).

use std::sync::Arc;

use narwhal_core::TableSchema;
use narwhal_domain::SchemaListing;
use narwhal_history::HistoryEntry;
use uuid::Uuid;

/// A request to perform a metadata operation in the background.
#[derive(Debug)]
pub enum MetaRequest {
    /// Fetch DDL for every table in the current session's schema listing.
    DumpSchemaAll {
        /// The stable tab id (see `Tab::id`) that initiated the request.
        /// Round-tripped through [`MetaUpdate::DumpSchemaReady`] so the
        /// reply lands on the originating tab even if other tabs were
        /// closed in the meantime (which shifts indices).  (Bug C5 fix.)
        tab_id: u64,
    },

    /// Refresh the schema listing for the current session.
    RefreshSchemas {
        /// The connection (session) id that originated the refresh.
        /// Round-tripped via [`MetaUpdate::SchemasRefreshed`] so a stale
        /// reply is dropped if the user switched sessions in the
        /// meantime.  (Bug H8 fix.)
        session_id: Uuid,
    },

    /// Load recent history entries from the journal.
    LoadHistory {
        /// Maximum number of entries to return.
        limit: usize,
    },
}

/// The result of a background metadata operation, delivered back to
/// the event loop via the meta channel.
#[derive(Debug)]
pub enum MetaUpdate {
    /// Response to [`MetaRequest::DumpSchemaAll`].
    DumpSchemaReady {
        /// The stable tab id (see `Tab::id`) that originated the request.
        /// The handler resolves this back to a current tab index, or
        /// drops the update if the tab was closed.
        tab_id: u64,
        /// Fetched table schemas, in the same order as the sidebar listing.
        tables: Vec<TableSchema>,
    },

    /// Response to [`MetaRequest::RefreshSchemas`].
    SchemasRefreshed {
        /// The session id that originated the refresh. The handler drops
        /// the update if the active session no longer matches.
        session_id: Uuid,
        /// Updated schema listing.
        schemas: Vec<SchemaListing>,
    },

    /// Response to [`MetaRequest::LoadHistory`].
    HistoryReady {
        /// Entries loaded from the journal.
        entries: Vec<HistoryEntry>,
    },

    /// A metadata operation failed.
    MetaFailed {
        /// Human-readable error message.
        message: String,
    },
}

/// Spawn a background task that performs the requested metadata operation
/// and sends the result back on `tx`.
///
/// `pool` is required for `DumpSchemaAll` and `RefreshSchemas`; it is
/// unused for `LoadHistory` (the caller may pass `None`).
pub fn spawn_meta_request(
    request: MetaRequest,
    pool: Option<narwhal_pool::Pool>,
    history: Option<Arc<narwhal_history::Journal>>,
    tx: tokio::sync::mpsc::Sender<MetaUpdate>,
) {
    tokio::spawn(async move {
        let update = match request {
            MetaRequest::DumpSchemaAll { tab_id } => {
                let Some(pool) = pool else {
                    let _ = tx
                        .send(MetaUpdate::MetaFailed {
                            message: "no active connection".into(),
                        })
                        .await;
                    return;
                };
                match dump_schema_all(&pool).await {
                    Ok(tables) => MetaUpdate::DumpSchemaReady { tab_id, tables },
                    Err(e) => MetaUpdate::MetaFailed {
                        message: format!("dump-schema failed: {e}"),
                    },
                }
            }
            MetaRequest::RefreshSchemas { session_id } => {
                let Some(pool) = pool else {
                    let _ = tx
                        .send(MetaUpdate::MetaFailed {
                            message: "no active connection".into(),
                        })
                        .await;
                    return;
                };
                match refresh_schemas_via_pool(&pool).await {
                    Ok(schemas) => MetaUpdate::SchemasRefreshed { session_id, schemas },
                    Err(e) => MetaUpdate::MetaFailed {
                        message: format!("refresh failed: {e}"),
                    },
                }
            }
            MetaRequest::LoadHistory { limit } => {
                let Some(journal) = history else {
                    let _ = tx
                        .send(MetaUpdate::MetaFailed {
                            message: "history disabled".into(),
                        })
                        .await;
                    return;
                };
                // M13: Journal::recent is async; it already off-loads
                // file I/O via spawn_blocking internally and returns
                // entries in chronological order (oldest first).
                match journal.recent(limit).await {
                    Ok(mut entries) => {
                        // The Ctrl+R modal shows newest first.
                        entries.reverse();
                        MetaUpdate::HistoryReady { entries }
                    }
                    Err(e) => MetaUpdate::MetaFailed {
                        message: format!("history read failed: {e}"),
                    },
                }
            }
        };
        let _ = tx.send(update).await;
    });
}

async fn dump_schema_all(
    pool: &narwhal_pool::Pool,
) -> narwhal_core::error::Result<Vec<TableSchema>> {
    let mut conn = pool
        .acquire()
        .await
        .map_err(|e| narwhal_core::Error::Connection(e.to_string()))?;
    let schemas = conn.list_all_tables().await?;
    let mut out = Vec::new();
    for (schema, tables) in &schemas {
        for table in tables {
            match conn.describe_table(&schema.name, &table.name).await {
                Ok(ts) => out.push(ts),
                Err(e) => {
                    tracing::warn!(
                        target: "narwhal::meta",
                        schema = %schema.name,
                        table = %table.name,
                        error = %e,
                        "describe_table failed during dump_schema all; skipping"
                    );
                }
            }
        }
    }
    Ok(out)
}

async fn refresh_schemas_via_pool(
    pool: &narwhal_pool::Pool,
) -> narwhal_core::error::Result<Vec<SchemaListing>> {
    let mut conn = pool
        .acquire()
        .await
        .map_err(|e| narwhal_core::Error::Connection(e.to_string()))?;
    let mut listing = conn.list_all_tables().await?;
    // If no schemas (e.g. SQLite returns "main" synthetic), still try to
    // list tables under the empty-string schema.
    if listing.is_empty() {
        if let Ok(tables) = conn.list_tables("").await {
            listing.push((
                narwhal_core::Schema {
                    name: String::new(),
                },
                tables,
            ));
        }
    }
    Ok(listing)
}
