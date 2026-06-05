//! Driver registry shared by every host that needs to address a
//! [`narwhal_core::DatabaseDriver`] by name (the TUI app, the MCP
//! server, the headless CLI). Concrete driver implementations are
//! pulled in by cargo features so a build can ship only the engines
//! it needs.
//!
//! Replaces the v1.x `narwhal-driver-registry` crate.

use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

use narwhal_core::{DynDatabaseDriver, Error, Result};

#[derive(Default, Clone)]
pub struct DriverRegistry {
    drivers: HashMap<&'static str, Arc<dyn DynDatabaseDriver>>,
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

    pub fn register<D: DynDatabaseDriver + 'static>(&mut self, driver: D) -> &mut Self {
        self.drivers.insert(driver.name(), Arc::new(driver));
        self
    }

    pub fn get(&self, name: &str) -> Result<Arc<dyn DynDatabaseDriver>> {
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
    /// the matching cargo feature.
    #[must_use]
    pub fn with_defaults() -> Self {
        // `mut` is conditional on at least one driver feature; with the
        // default feature set (no drivers) the variable is never
        // assigned and clippy would warn.
        #[cfg_attr(
            not(any(
                feature = "postgres",
                feature = "sqlite",
                feature = "mysql",
                feature = "duckdb",
                feature = "clickhouse",
                feature = "mssql",
            )),
            allow(unused_mut)
        )]
        let mut registry = Self::new();
        #[cfg(feature = "postgres")]
        registry.register(crate::postgres::PostgresDriver::new());
        #[cfg(feature = "sqlite")]
        registry.register(crate::sqlite::SqliteDriver::new());
        #[cfg(feature = "mysql")]
        registry.register(crate::mysql::MysqlDriver::new());
        #[cfg(feature = "duckdb")]
        registry.register(crate::duckdb::DuckdbDriver::new());
        #[cfg(feature = "clickhouse")]
        registry.register(crate::clickhouse::ClickhouseDriver::new());
        #[cfg(feature = "mssql")]
        registry.register(crate::mssql::MssqlDriver::new());
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

    #[cfg(feature = "sqlite")]
    #[test]
    fn with_defaults_registers_enabled_drivers() {
        let registry = DriverRegistry::with_defaults();
        assert!(registry.contains("sqlite"));
    }

    #[cfg(feature = "mssql")]
    #[test]
    fn with_defaults_registers_mssql() {
        let registry = DriverRegistry::with_defaults();
        assert!(registry.contains("mssql"));
    }
}
