//! Driver registry shared by every host that needs to address a
//! [`DatabaseDriver`] by name (the TUI app, the MCP server, the headless
//! CLI). Concrete driver implementations are pulled in by cargo features
//! so a build can ship only the engines it needs.

#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

use narwhal_core::{DatabaseDriver, Error, Result};

#[derive(Default, Clone)]
pub struct DriverRegistry {
    drivers: HashMap<&'static str, Arc<dyn DatabaseDriver>>,
}

impl fmt::Debug for DriverRegistry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DriverRegistry")
            .field("drivers", &self.drivers.keys().collect::<Vec<_>>())
            .finish()
    }
}

impl DriverRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register<D: DatabaseDriver + 'static>(&mut self, driver: D) -> &mut Self {
        self.drivers.insert(driver.name(), Arc::new(driver));
        self
    }

    pub fn get(&self, name: &str) -> Result<Arc<dyn DatabaseDriver>> {
        self.drivers
            .get(name)
            .cloned()
            .ok_or_else(|| Error::UnknownDriver(name.into()))
    }

    #[must_use]
    pub fn contains(&self, name: &str) -> bool {
        self.drivers.contains_key(name)
    }

    pub fn names(&self) -> impl Iterator<Item = &'static str> + '_ {
        self.drivers.keys().copied()
    }

    pub fn is_empty(&self) -> bool {
        self.drivers.is_empty()
    }

    pub fn len(&self) -> usize {
        self.drivers.len()
    }

    /// Registry preloaded with every driver compiled into this build via
    /// the `driver-*` cargo features.
    #[must_use]
    pub fn with_defaults() -> Self {
        let mut registry = Self::new();
        #[cfg(feature = "driver-postgres")]
        registry.register(narwhal_driver_postgres::PostgresDriver::new());
        #[cfg(feature = "driver-sqlite")]
        registry.register(narwhal_driver_sqlite::SqliteDriver::new());
        #[cfg(feature = "driver-mysql")]
        registry.register(narwhal_driver_mysql::MysqlDriver::new());
        #[cfg(feature = "driver-duckdb")]
        registry.register(narwhal_driver_duckdb::DuckdbDriver::new());
        #[cfg(feature = "driver-clickhouse")]
        registry.register(narwhal_driver_clickhouse::ClickhouseDriver::new());
        registry
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_registry_returns_unknown_driver() {
        let registry = DriverRegistry::new();
        assert!(registry.is_empty());
        assert!(registry.get("postgres").is_err());
    }

    #[cfg(feature = "driver-sqlite")]
    #[test]
    fn with_defaults_registers_enabled_drivers() {
        let registry = DriverRegistry::with_defaults();
        assert!(registry.contains("sqlite"));
    }
}
