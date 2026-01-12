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
use secrecy::ExposeSecret;

use crate::error::McpError;
use crate::registry::DriverRegistry;

/// State accessible to tool implementations.
///
/// Cheap to clone — every field is either an `Arc` or `Copy`-ish.
#[derive(Clone)]
pub struct ServerContext {
    drivers: Arc<DriverRegistry>,
    connections: Arc<ConnectionsFile>,
    credentials: Arc<dyn CredentialStore>,
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
        }
    }

    pub fn connections(&self) -> &ConnectionsFile {
        &self.connections
    }

    /// Resolve a connection by user-facing name and dial it.
    ///
    /// The caller is responsible for `close()`-ing the returned connection.
    /// We deliberately do not return an `Arc<Mutex<…>>` long-lived handle
    /// — every MCP call is short and the per-call dial cost is negligible
    /// compared to the network round-trips that follow.
    pub async fn open_connection(&self, name: &str) -> Result<Box<dyn Connection>, McpError> {
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
}
