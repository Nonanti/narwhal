//! Driver registry used by the MCP server.
//!
//! This deliberately duplicates the few lines of registry plumbing from
//! `narwhal-app` rather than depending on the whole TUI crate — pulling in
//! `narwhal-app` would drag ratatui, crossterm, arboard and a dozen other
//! UI deps into a headless server binary for no benefit.

use std::collections::HashMap;
use std::sync::Arc;

use narwhal_core::{DatabaseDriver, Error};

use crate::error::McpError;

#[derive(Default, Clone)]
pub struct DriverRegistry {
    drivers: HashMap<&'static str, Arc<dyn DatabaseDriver>>,
}

impl DriverRegistry {
    pub fn register<D: DatabaseDriver + 'static>(&mut self, driver: D) {
        self.drivers.insert(driver.name(), Arc::new(driver));
    }

    pub fn get(&self, name: &str) -> Result<Arc<dyn DatabaseDriver>, McpError> {
        self.drivers
            .get(name)
            .cloned()
            .ok_or_else(|| McpError::Connection(Error::UnknownDriver(name.into())))
    }

    /// Registry preloaded with every driver bundled with narwhal.
    pub fn with_defaults() -> Self {
        let mut registry = Self::default();
        registry.register(narwhal_driver_postgres::PostgresDriver::new());
        registry.register(narwhal_driver_sqlite::SqliteDriver::new());
        registry.register(narwhal_driver_mysql::MysqlDriver::new());
        registry.register(narwhal_driver_duckdb::DuckdbDriver::new());
        registry.register(narwhal_driver_clickhouse::ClickhouseDriver::new());
        registry
    }
}
