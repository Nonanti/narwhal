//! Bundled database drivers for narwhal.
//!
//! Each engine (`postgres`, `mysql`, `sqlite`, `duckdb`, `clickhouse`,
//! `mssql`) lives behind a cargo feature of the same name. Enabling the
//! `all-drivers` umbrella turns them all on; the `narwhaldb` binary
//! relies on this. Library consumers pick what they need:
//!
//! ```toml
//! narwhal-drivers = { version = "1", default-features = false, features = ["postgres", "sqlite"] }
//! ```
//!
//! The crate also re-exports [`registry::DriverRegistry`] as
//! [`DriverRegistry`], which replaces the v1.x `narwhal-driver-registry`
//! crate.

#![forbid(unsafe_code)]

#[cfg(feature = "clickhouse")]
pub mod clickhouse;
#[cfg(feature = "duckdb")]
pub mod duckdb;
#[cfg(feature = "mssql")]
pub mod mssql;
#[cfg(feature = "mysql")]
pub mod mysql;
#[cfg(feature = "postgres")]
pub mod postgres;
#[cfg(feature = "sqlite")]
pub mod sqlite;

pub mod registry;

pub use registry::DriverRegistry;

/// Convenience constructor for a registry preloaded with every driver
/// compiled into this build. Equivalent to
/// [`DriverRegistry::with_defaults`]; kept as a free function so the
/// historical `narwhal_drivers::Registry::new()` call shape
/// has a minimally-disruptive migration target.
#[must_use]
pub fn registry() -> DriverRegistry {
    DriverRegistry::with_defaults()
}
