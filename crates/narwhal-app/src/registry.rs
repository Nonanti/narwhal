use std::collections::HashMap;
use std::sync::Arc;

use narwhal_core::{DatabaseDriver, Error, Result};

/// Lookup table of registered [`DatabaseDriver`] implementations.
#[derive(Default, Clone)]
pub struct DriverRegistry {
    drivers: HashMap<&'static str, Arc<dyn DatabaseDriver>>,
}

impl DriverRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register<D: DatabaseDriver + 'static>(&mut self, driver: D) {
        self.drivers.insert(driver.name(), Arc::new(driver));
    }

    pub fn get(&self, name: &str) -> Result<Arc<dyn DatabaseDriver>> {
        self.drivers
            .get(name)
            .cloned()
            .ok_or_else(|| Error::UnknownDriver(name.into()))
    }

    pub fn contains(&self, name: &str) -> bool {
        self.drivers.contains_key(name)
    }

    pub fn names(&self) -> impl Iterator<Item = &&'static str> {
        self.drivers.keys()
    }

    /// Registry preloaded with every driver bundled with narwhal.
    pub fn with_defaults() -> Self {
        let mut registry = Self::new();
        registry.register(narwhal_driver_postgres::PostgresDriver::new());
        registry.register(narwhal_driver_sqlite::SqliteDriver::new());
        registry.register(narwhal_driver_mysql::MysqlDriver::new());
        registry.register(narwhal_driver_duckdb::DuckdbDriver::new());
        registry.register(narwhal_driver_clickhouse::ClickhouseDriver::new());
        registry
    }
}
