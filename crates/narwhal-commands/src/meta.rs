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

use narwhal_config::CredentialStore;
use narwhal_core::{ConnectionConfig, DatabaseDriver, TableSchema};
use narwhal_domain::SchemaListing;
use narwhal_history::HistoryEntry;
use secrecy::{ExposeSecret, SecretString};
use uuid::Uuid;

use crate::session::{Session, SessionOpenOptions};

/// A request to perform a metadata operation in the background.
#[non_exhaustive]
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

    /// Open a session in the background (keyring lookup + dial + initial
    /// schema refresh) and deliver the result via
    /// [`MetaUpdate::SessionOpened`]. The event loop hands the request
    /// off so the user sees `connecting to …` immediately instead of a
    /// frozen UI while a slow DNS / TLS handshake completes.  (Bug H7
    /// fix — the highest-impact `block_in_place` call.)
    OpenSession {
        /// Driver instance (cloneable `Arc`) for the connection.
        driver: Arc<dyn DatabaseDriver>,
        /// Connection metadata. Boxed to keep the enum slim —
        /// `ConnectionConfig` carries a `ConnectionParams` blob that
        /// is the biggest variant by far.
        config: Box<ConnectionConfig>,
        /// Optional pre-resolved password. When `None`, the worker
        /// consults the credential store and the pgpass / env fallback.
        password_hint: Option<String>,
        /// Pass through to [`Session::open_with`]. The CLI flips
        /// `skip_pre_connect` under `--read-only`.
        opts: SessionOpenOptions,
    },

    /// Sprint 9 (H7): a `:test <name|url>` request. The worker
    /// resolves credentials, dials the database, drops the session,
    /// and reports the outcome via [`MetaUpdate::TestCompleted`].
    /// Eliminates the `block_in_place` that previously froze the UI
    /// for the full TCP / TLS handshake when the user invoked `:test`.
    TestConnection {
        /// Driver instance for the connection under test.
        driver: Arc<dyn DatabaseDriver>,
        /// Connection metadata. Boxed for the same reason as
        /// `OpenSession::config`.
        config: Box<ConnectionConfig>,
        /// Optional pre-resolved password (parsed from the DSN form).
        /// `None` triggers the credential-store + pgpass lookup.
        password: Option<String>,
        /// Sandbox flag mirrored from `OpenSession`.
        opts: SessionOpenOptions,
        /// Label shown in the status bar ("test ok: <label>" /
        /// "test failed: <label>").
        label: String,
    },
}

impl std::fmt::Debug for MetaRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DumpSchemaAll { tab_id } => f
                .debug_struct("DumpSchemaAll")
                .field("tab_id", tab_id)
                .finish(),
            Self::RefreshSchemas { session_id } => f
                .debug_struct("RefreshSchemas")
                .field("session_id", session_id)
                .finish(),
            Self::LoadHistory { limit } => {
                f.debug_struct("LoadHistory").field("limit", limit).finish()
            }
            Self::OpenSession { config, opts, .. } => f
                .debug_struct("OpenSession")
                .field("config.name", &config.name)
                .field("opts", opts)
                .finish(),
            Self::TestConnection { label, opts, .. } => f
                .debug_struct("TestConnection")
                .field("label", label)
                .field("opts", opts)
                .finish(),
        }
    }
}

/// The result of a background metadata operation, delivered back to
/// the event loop via the meta channel.
#[derive(Debug)]
#[non_exhaustive]
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

    /// Response to [`MetaRequest::OpenSession`].
    SessionOpened {
        /// The id of the [`ConnectionConfig`] we tried to open. The
        /// event loop matches this against the pending-open ledger so a
        /// stale reply (user opened another connection in the meantime)
        /// can be silently dropped instead of clobbering the current
        /// session.
        config_id: Uuid,
        /// `Ok(session)` on success. The handler swaps it into
        /// `self.session` and runs the standard post-open wiring
        /// (sidebar rebuild, plugin pool publish, status line). On
        /// `Err`, the message goes to the status line.
        result: Result<Box<Session>, String>,
    },

    /// A metadata operation failed.
    MetaFailed {
        /// Human-readable error message.
        message: String,
    },

    /// Response to [`MetaRequest::TestConnection`]. `Ok(driver_name)`
    /// indicates a successful round-trip; `Err(message)` carries the
    /// engine-level reason. The status bar applies the verdict.
    TestCompleted {
        /// Label echoed back from the request.
        label: String,
        /// `Ok(driver_name)` on success, `Err(message)` on failure.
        result: Result<String, String>,
    },

    /// Bug-fix: a keyring lookup initiated by `start_wizard_edit`
    /// has completed. The UI handler injects the secret into the
    /// currently-open wizard, *but only if* the wizard is still
    /// open against the same `connection_id` — if the user moved
    /// on (closed the wizard, started editing a different
    /// connection, opened a fresh "new" wizard) the reply is
    /// silently dropped. The secret is wrapped in `SecretString` so
    /// it stays zeroized on drop even when sitting in this enum.
    CredentialReady {
        /// The connection whose password was being fetched.
        connection_id: Uuid,
        /// `Some(secret)` when the keyring had a stored secret,
        /// `None` when the keyring lookup succeeded but the slot
        /// was empty (no error — user has never saved one).
        password: Option<SecretString>,
    },

    /// Bug-fix: outcome of an async keyring delete initiated by
    /// `:forget <name>`. The UI handler shows a real success /
    /// failure status instead of the previous "(best-effort)"
    /// jargon.
    ForgetCompleted {
        /// Connection name echoed back for the status line.
        name: String,
        /// `Ok(())` on a successful delete, `Err(message)` when the
        /// keyring rejected the operation. "Slot not found" is a
        /// success — the user already had nothing stored — so the
        /// worker normalises that case to `Ok`.
        result: Result<(), String>,
    },

    /// Sprint 11 (Opus M1): sidebar `inject_ddl` result. The handler
    /// resolves `tab_id` to the current index (C5 stable handle),
    /// drops the update if the originating tab was closed, and
    /// otherwise pastes `ddl` into the tab's editor buffer.
    InjectDdlReady {
        /// Stable tab id of the tab that requested the DDL.
        tab_id: u64,
        /// Schema-qualified object the DDL describes (for the status
        /// bar message).
        schema: String,
        /// Object name the DDL describes.
        name: String,
        /// Rendered DDL text. The driver decides the dialect; the
        /// UI does not re-render.
        ddl: String,
    },
}

/// Spawn a background task that performs the requested metadata operation
/// and sends the result back on `tx`.
///
/// `pool` is required for `DumpSchemaAll` and `RefreshSchemas`; it is
/// unused for `LoadHistory` and `OpenSession` (the caller may pass
/// `None`). `credentials` is consulted only by `OpenSession`.
pub fn spawn_meta_request(
    request: MetaRequest,
    pool: Option<narwhal_pool::Pool>,
    history: Option<Arc<narwhal_history::Journal>>,
    credentials: Option<Arc<dyn CredentialStore>>,
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
                    Ok(schemas) => MetaUpdate::SchemasRefreshed {
                        session_id,
                        schemas,
                    },
                    Err(e) => MetaUpdate::MetaFailed {
                        message: format!("refresh failed: {e}"),
                    },
                }
            }
            MetaRequest::OpenSession {
                driver,
                config,
                password_hint,
                opts,
            } => {
                let config_id = config.id;
                // Resolve credentials inside the task so the keyring
                // round-trip does not stall the event loop.
                let password = match password_hint {
                    Some(p) => Some(p),
                    None => resolve_password(credentials.as_deref(), &config).await,
                };
                let result = match Session::open_with(
                    Arc::clone(&driver),
                    (*config).clone(),
                    password,
                    opts,
                )
                .await
                {
                    Ok(mut session) => {
                        if let Err(error) = session.refresh_schemas().await {
                            tracing::debug!(
                                target: "narwhal::meta",
                                error = %error,
                                "initial schema refresh failed after open; continuing"
                            );
                        }
                        Ok(Box::new(session))
                    }
                    Err(error) => Err(error.to_string()),
                };
                MetaUpdate::SessionOpened { config_id, result }
            }
            MetaRequest::TestConnection {
                driver,
                config,
                password,
                opts,
                label,
            } => {
                let resolved = match password {
                    Some(p) => Some(p),
                    None => resolve_password(credentials.as_deref(), &config).await,
                };
                let result = match Session::open_with(
                    Arc::clone(&driver),
                    (*config).clone(),
                    resolved,
                    opts,
                )
                .await
                {
                    Ok(session) => {
                        let driver_name = session.driver.name().to_owned();
                        // Drop the session immediately — we only
                        // needed to know the handshake worked.
                        drop(session);
                        Ok(driver_name)
                    }
                    Err(e) => Err(e.to_string()),
                };
                MetaUpdate::TestCompleted { label, result }
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

async fn resolve_password(
    credentials: Option<&dyn CredentialStore>,
    config: &ConnectionConfig,
) -> Option<String> {
    if let Some(store) = credentials {
        if let Ok(Some(secret)) = store.get(config.id).await {
            return Some(secret.expose_secret().to_owned());
        }
    }
    narwhal_config::resolve_fallback_password(&config.driver, &config.params)
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
