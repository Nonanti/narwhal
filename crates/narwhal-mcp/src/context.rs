//! Shared, read-only context handed to every tool invocation.
//!
//! Tools never construct connections themselves — they ask the context.
//! Centralising connection construction here means the credential
//! resolution chain (keyring → pgpass → env) and the SSH tunnel lifecycle
//! stay consistent with the TUI path and we only have one place to bolt
//! the future workspace.toml scoping logic onto.

use std::sync::Arc;

use narwhal_config::{ConnectionsFile, CredentialStore};
use narwhal_core::{Connection, ConnectionConfig};
use narwhal_history::{HistoryEntry, Journal};
use secrecy::ExposeSecret;

use crate::error::McpError;
use crate::registry::DriverRegistry;
use crate::workspace::Workspace;

/// Free-form source tag written to every `HistoryEntry` the MCP server
/// produces. The TUI tags itself implicitly with `None`; auditors search
/// for `source = "mcp"` to isolate agent-driven traffic.
pub const AUDIT_SOURCE: &str = "mcp";

/// State accessible to tool implementations.
///
/// Cheap to clone — every field is either an `Arc` or `Copy`-ish.
#[derive(Clone)]
pub struct ServerContext {
    drivers: Arc<DriverRegistry>,
    connections: Arc<ConnectionsFile>,
    credentials: Arc<dyn CredentialStore>,
    /// Optional. When `Some`, every tool that touches the database appends
    /// an audited entry tagged `source = "mcp"` so the operator can audit
    /// agent activity offline. When `None` (typically in unit tests), the
    /// audit calls are no-ops.
    journal: Option<Arc<Journal>>,
    /// Optional. When `Some`, every connection-touching tool consults the
    /// workspace ACL before dispatch. `None` means "no workspace file
    /// found" — expose every connection from `connections.toml` and
    /// honour the per-call `read_only` flag without further restriction.
    workspace: Option<Arc<Workspace>>,
    /// Global CLI-level read-only override. When `true`, `writes_allowed()`
    /// returns `false` regardless of workspace ACL — `narwhal --read-only mcp`
    /// must trump any permissive `workspace.toml` so the user-facing flag
    /// keeps its promise on the MCP surface (the highest-risk path).
    force_read_only: bool,
}

impl ServerContext {
    pub fn new(
        drivers: Arc<DriverRegistry>,
        connections: Arc<ConnectionsFile>,
        credentials: Arc<dyn CredentialStore>,
    ) -> Self {
        Self {
            drivers,
            connections,
            credentials,
            journal: None,
            workspace: None,
            force_read_only: false,
        }
    }

    /// Force read-only mode regardless of workspace ACL.
    ///
    /// Wired from `--read-only` on the CLI. Once set, `writes_allowed()`
    /// always returns `false` so every tool rejects `read_only=false`.
    #[must_use]
    pub const fn with_force_read_only(mut self, force: bool) -> Self {
        self.force_read_only = force;
        self
    }

    /// Attach an audit journal. Returns `self` so the constructor can be
    /// used fluently in the binary entry point.
    #[must_use]
    pub fn with_journal(mut self, journal: Arc<Journal>) -> Self {
        self.journal = Some(journal);
        self
    }

    /// Attach a workspace file. When attached, ACL checks are enforced
    /// on every connection-touching tool call.
    #[must_use]
    pub fn with_workspace(mut self, workspace: Arc<Workspace>) -> Self {
        self.workspace = Some(workspace);
        self
    }

    pub fn connections(&self) -> &ConnectionsFile {
        &self.connections
    }

    /// Connections visible to the agent under the current workspace.
    ///
    /// Tools that enumerate connections (`list_connections`) call this
    /// instead of touching `connections()` directly so the response
    /// already reflects the workspace allow-list.
    pub fn visible_connections(&self) -> Vec<&ConnectionConfig> {
        self.connections
            .connections
            .iter()
            .filter(|c| self.is_connection_allowed(&c.name))
            .collect()
    }

    /// True when `name` is visible under the current workspace's ACL.
    /// `true` is returned when no workspace is attached — absence of a
    /// workspace file means "everything goes", matching the documented
    /// default in the README.
    pub fn is_connection_allowed(&self, name: &str) -> bool {
        self.workspace
            .as_ref()
            .map_or(true, |ws| ws.connection_allowed(name))
    }

    /// True when writes are permitted in the current workspace.
    ///
    /// Returns `false` when the global `--read-only` flag is set, even
    /// if the workspace ACL would otherwise permit writes. Returns
    /// `true` when no workspace is attached *and* the flag is unset.
    pub fn writes_allowed(&self) -> bool {
        if self.force_read_only {
            return false;
        }
        self.workspace
            .as_ref()
            .map_or(true, |ws| ws.file.allow_writes)
    }

    /// True when the global `--read-only` CLI flag is in effect.
    /// Surfaces in error messages so the agent learns *why* writes
    /// are off instead of guessing it's a workspace policy.
    pub const fn force_read_only(&self) -> bool {
        self.force_read_only
    }

    /// Borrow the workspace if one is attached. Tools usually go through
    /// the helper methods above instead of inspecting the workspace
    /// directly.
    pub fn workspace(&self) -> Option<&Workspace> {
        self.workspace.as_deref()
    }

    /// Resolve a connection by user-facing name and dial it.
    ///
    /// The caller is responsible for `close()`-ing the returned connection.
    /// We deliberately do not return an `Arc<Mutex<…>>` long-lived handle
    /// — every MCP call is short and the per-call dial cost is negligible
    /// compared to the network round-trips that follow.
    pub async fn open_connection(&self, name: &str) -> Result<Box<dyn Connection>, McpError> {
        if !self.is_connection_allowed(name) {
            // Workspace ACLs reject up-front so the agent sees a clear
            // "not exposed" signal instead of a vague driver error
            // after the dial. We re-use `UnknownConnection` because
            // that's exactly how the agent should treat the rejection:
            // re-call `list_connections` and pick from the visible set.
            return Err(McpError::UnknownConnection(name.to_string()));
        }
        let config = self.find_by_name(name)?;
        let driver = self.drivers.get(&config.driver)?;
        let password = self.resolve_password(&config).await?;
        let connection = driver.connect(&config, password.as_deref()).await?;
        Ok(connection)
    }

    fn find_by_name(&self, name: &str) -> Result<ConnectionConfig, McpError> {
        self.connections
            .connections
            .iter()
            .find(|c| c.name == name)
            .cloned()
            .ok_or_else(|| McpError::UnknownConnection(name.to_string()))
    }

    /// Resolution order mirrors the TUI's `:open` path so the MCP server
    /// never sees a credential the TUI wouldn't: keyring first, then the
    /// `~/.pgpass` / env-var fallback. Failures in the keyring leg are not
    /// fatal — they just fall through to the secondary resolvers.
    async fn resolve_password(
        &self,
        config: &ConnectionConfig,
    ) -> Result<Option<String>, McpError> {
        if let Ok(Some(secret)) = self.credentials.get(config.id).await {
            return Ok(Some(secret.expose_secret().to_string()));
        }
        Ok(narwhal_config::pgpass::resolve_password(
            &config.driver,
            &config.params,
        ))
    }

    /// Append an audit entry for a query the agent is about to run.
    ///
    /// The entry carries `source = "mcp"` so the operator can grep for
    /// agent-issued statements after the fact. We resolve the connection
    /// metadata best-effort and swallow journal-write failures: an
    /// unwriteable history file must not break the request path.
    pub async fn audit_query(&self, connection_name: &str, sql: &str, read_only: bool) {
        let Some(journal) = self.journal.as_ref() else {
            return;
        };
        // We always log the SQL the agent supplied; the journal already
        // redacts known secret patterns (CREATE USER ... PASSWORD '…' etc.)
        // before writing, so we don't need to do it here.
        let mut entry = HistoryEntry::success(sql).with_source(AUDIT_SOURCE);
        if let Some(config) = self
            .connections
            .connections
            .iter()
            .find(|c| c.name == connection_name)
        {
            entry = entry
                .with_connection(config.id, &config.name)
                .with_driver(&config.driver);
        }
        // Hint the agent's intent (read-only/full) by appending a comment
        // suffix — the entry struct does not have a dedicated field and we
        // don't want to grow the schema for a single bit.
        if !read_only {
            entry.sql = format!("-- mcp: read_only=false\n{}", entry.sql);
        }
        if let Err(error) = journal.append(&entry).await {
            tracing::warn!(error = %error, "MCP audit append failed");
        }
    }
}
