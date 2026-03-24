// Trait definitions intentionally keep explicit `'a` lifetimes on the
// dyn-safe sibling methods: every borrowed parameter shares the same
// lifetime as the returned `BoxFuture`, which elision cannot express
// (multi-input borrows would each get an independent anonymous
// lifetime).
#![allow(clippy::needless_lifetimes, clippy::elidable_lifetime_names)]

use crate::future::BoxFuture;
use std::future::Future;

use crate::connection::{ConnectionConfig, DynConnection};
use crate::error::Result;

/// Factory for [`crate::Connection`] instances of a particular database engine.
///
/// Drivers are registered at application start-up keyed by
/// [`DatabaseDriver::name`] and are referenced from configuration files by
/// that same identifier.
///
/// # Trait shape
///
/// Like [`crate::Connection`], this trait uses native `async fn` in trait
/// (RPITIT). For dyn-object use sites (the driver registry hands
/// drivers out as `Arc<dyn DynDatabaseDriver>`), implement via the
/// blanket [`DynDatabaseDriver`] wrapper.
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
    fn connect(
        &self,
        config: &ConnectionConfig,
        password: Option<&str>,
    ) -> impl Future<Output = Result<Box<dyn DynConnection>>> + Send;
}

/// Dyn-safe sibling of [`DatabaseDriver`].
///
/// See [`crate::DynConnection`] for the rationale. Driver registries
/// and any other site that holds a trait object use
/// `Arc<dyn DynDatabaseDriver>`.
pub trait DynDatabaseDriver: Send + Sync {
    fn name(&self) -> &'static str;

    fn display_name(&self) -> &'static str;

    fn validate(&self, config: &ConnectionConfig) -> Vec<String>;

    fn connect<'a>(
        &'a self,
        config: &'a ConnectionConfig,
        password: Option<&'a str>,
    ) -> BoxFuture<'a, Result<Box<dyn DynConnection>>>;
}

impl<T> DynDatabaseDriver for T
where
    T: DatabaseDriver + 'static,
{
    fn name(&self) -> &'static str {
        <Self as DatabaseDriver>::name(self)
    }

    fn display_name(&self) -> &'static str {
        <Self as DatabaseDriver>::display_name(self)
    }

    fn validate(&self, config: &ConnectionConfig) -> Vec<String> {
        <Self as DatabaseDriver>::validate(self, config)
    }

    fn connect<'a>(
        &'a self,
        config: &'a ConnectionConfig,
        password: Option<&'a str>,
    ) -> BoxFuture<'a, Result<Box<dyn DynConnection>>> {
        Box::pin(<Self as DatabaseDriver>::connect(self, config, password))
    }
}
