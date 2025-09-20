use async_trait::async_trait;

use crate::connection::{Connection, ConnectionConfig};
use crate::error::Result;

/// Factory for [`Connection`] instances of a particular database.
///
/// Drivers register themselves with a `DriverRegistry` (see narwhal-app)
/// keyed by [`DatabaseDriver::name`].
#[async_trait]
pub trait DatabaseDriver: Send + Sync {
    /// Stable identifier used in config files (`"postgres"`, `"sqlite"`, …).
    fn name(&self) -> &'static str;

    /// Human-readable display name (`"PostgreSQL"`, `"SQLite"`).
    fn display_name(&self) -> &'static str;

    /// Validate the config without actually connecting. Returns a list of
    /// human-readable problems; empty means OK.
    fn validate(&self, config: &ConnectionConfig) -> Vec<String> {
        let _ = config;
        Vec::new()
    }

    /// Open a connection. The driver also handles credential lookup via the
    /// `password` argument (passed in by the caller after fetching from keyring).
    async fn connect(
        &self,
        config: &ConnectionConfig,
        password: Option<&str>,
    ) -> Result<Box<dyn Connection>>;
}
