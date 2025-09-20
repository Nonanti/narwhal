use std::collections::HashMap;
use std::sync::Arc;

use narwhal_core::{DatabaseDriver, Error, Result};

/// Lookup table for available [`DatabaseDriver`] implementations.
///
/// New drivers register themselves at startup via [`DriverRegistry::register`].
#[derive(Default, Clone)]
pub struct DriverRegistry {
    drivers: HashMap<&'static str, Arc<dyn DatabaseDriver>>,
}

impl DriverRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register<D: DatabaseDriver + 'static>(&mut self, driver: D) {
        let name = driver.name();
        self.drivers.insert(name, Arc::new(driver));
    }

    pub fn get(&self, name: &str) -> Result<Arc<dyn DatabaseDriver>> {
        self.drivers
            .get(name)
            .cloned()
            .ok_or_else(|| Error::UnknownDriver(name.into()))
    }

    pub fn names(&self) -> impl Iterator<Item = &&'static str> {
        self.drivers.keys()
    }

    /// Convenience: register all drivers that ship with narwhal by default.
    pub fn with_defaults() -> Self {
        let mut me = Self::new();
        me.register(narwhal_driver_postgres::PostgresDriver::new());
        me.register(narwhal_driver_sqlite::SqliteDriver::new());
        me
    }
}
