use thiserror::Error;

pub type Result<T> = std::result::Result<T, Error>;

/// Boxed driver-level cause carried alongside a high-level
/// classification. `Send + Sync + 'static` so the error can cross task
/// boundaries (tokio spawn, mpsc channels) without contortions.
pub type Source = Box<dyn std::error::Error + Send + Sync + 'static>;

/// Error returned from the core abstractions and from driver implementations.
///
/// M1: each high-level variant now has a `*WithSource` sibling that
/// preserves the underlying driver error via `thiserror`'s `#[source]`
/// chain. Callers that need to discriminate driver-specific conditions
/// (e.g. `tokio_postgres::Error::is_closed()`) can downcast via
/// `std::error::Error::source()` and `Any::downcast_ref` without
/// requiring narwhal-core to depend on every driver crate.
///
/// The string-only variants are retained for backwards compatibility
/// with drivers that don't yet carry the source through; both render
/// identically via `Display`, so log lines and user-facing messages
/// are unchanged.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum Error {
    #[error("connection failed: {0}")]
    Connection(String),

    #[error("connection failed: {msg}")]
    ConnectionWithSource {
        msg: String,
        #[source]
        source: Source,
    },

    #[error("authentication failed")]
    Authentication,

    #[error("query failed: {0}")]
    Query(String),

    #[error("query failed: {msg}")]
    QueryWithSource {
        msg: String,
        #[source]
        source: Source,
    },

    #[error("driver `{0}` is not registered")]
    UnknownDriver(String),

    #[error("unsupported type: {0}")]
    UnsupportedType(String),

    #[error("feature not supported by this driver: {0}")]
    Unsupported(String),

    #[error("schema error: {0}")]
    Schema(String),

    #[error("configuration error: {0}")]
    Config(String),

    #[error("configuration error: {msg}")]
    ConfigWithSource {
        msg: String,
        #[source]
        source: Source,
    },

    #[error("operation was cancelled")]
    Cancelled,

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("{0}")]
    Other(String),
}

impl Error {
    pub fn other(msg: impl Into<String>) -> Self {
        Self::Other(msg.into())
    }

    pub fn unsupported(msg: impl Into<String>) -> Self {
        Self::Unsupported(msg.into())
    }

    /// Build a connection error that preserves the underlying driver
    /// error in the `source()` chain.
    pub fn connection_with(
        msg: impl Into<String>,
        source: impl Into<Source>,
    ) -> Self {
        Self::ConnectionWithSource {
            msg: msg.into(),
            source: source.into(),
        }
    }

    /// Build a query error that preserves the underlying driver error
    /// in the `source()` chain.
    pub fn query_with(msg: impl Into<String>, source: impl Into<Source>) -> Self {
        Self::QueryWithSource {
            msg: msg.into(),
            source: source.into(),
        }
    }

    /// Build a configuration error that preserves the underlying parse
    /// / validation error in the `source()` chain.
    pub fn config_with(
        msg: impl Into<String>,
        source: impl Into<Source>,
    ) -> Self {
        Self::ConfigWithSource {
            msg: msg.into(),
            source: source.into(),
        }
    }

    /// Walk the `std::error::Error::source()` chain looking for a
    /// concrete driver error type. Convenience wrapper around the
    /// repeated downcast pattern.
    pub fn find_source<T: std::error::Error + 'static>(&self) -> Option<&T> {
        let mut current: Option<&(dyn std::error::Error + 'static)> =
            std::error::Error::source(self);
        while let Some(err) = current {
            if let Some(target) = err.downcast_ref::<T>() {
                return Some(target);
            }
            current = err.source();
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Error)]
    #[error("driver-specific boom: code={0}")]
    struct FakeDriverError(u32);

    #[test]
    fn connection_with_preserves_source() {
        let driver = FakeDriverError(42);
        let err = Error::connection_with("failed to handshake", driver);
        assert!(matches!(err, Error::ConnectionWithSource { .. }));
        assert_eq!(err.to_string(), "connection failed: failed to handshake");
        let found = err.find_source::<FakeDriverError>().expect("chain");
        assert_eq!(found.0, 42);
    }

    #[test]
    fn query_with_chain_is_walkable() {
        let err = Error::query_with("select bombed", FakeDriverError(7));
        let mut chain: Option<&(dyn std::error::Error + 'static)> =
            std::error::Error::source(&err);
        let mut hops = 0;
        while let Some(c) = chain {
            hops += 1;
            chain = c.source();
        }
        assert!(hops >= 1);
    }

    #[test]
    fn legacy_string_variants_unchanged() {
        // The existing tuple variants must keep working so drivers can
        // migrate gradually.
        let err = Error::Connection("plain".into());
        assert_eq!(err.to_string(), "connection failed: plain");
        assert!(std::error::Error::source(&err).is_none());
    }
}
