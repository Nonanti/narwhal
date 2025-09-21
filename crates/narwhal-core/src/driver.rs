use async_trait::async_trait;

use crate::connection::{Connection, ConnectionConfig};
use crate::error::Result;

/// Factory for [`Connection`] instances of a particular database engine.
///
/// Drivers are registered at application start-up keyed by
/// [`DatabaseDriver::name`] and are referenced from configuration files by
/// that same identifier.
#[async_trait]
pub trait DatabaseDriver: Send + Sync {
    /// Stable identifier persisted to disk (e.g. `"postgres"`, `"sqlite"`).
    fn name(&self) -> &'static str;

    /// Human-readable label shown in the user interface.
    fn display_name(&self) -> &'static str;

    /// Validate `config` without contacting the server.
    ///
    /// Returns a list of human-readable problems. An empty vector indicates
    /// the configuration is structurally sound.
    fn validate(&self, config: &ConnectionConfig) -> Vec<String> {
        let _ = config;
        Vec::new()
    }

    /// Establish a new connection.
    ///
    /// `password` is resolved by the caller from the configured credential
    /// store. Drivers that do not require authentication ignore the argument.
    async fn connect(
        &self,
        config: &ConnectionConfig,
        password: Option<&str>,
    ) -> Result<Box<dyn Connection>>;
}
