use thiserror::Error;

pub type Result<T> = std::result::Result<T, Error>;

/// Error returned from the core abstractions and from driver implementations.
#[derive(Debug, Error)]
pub enum Error {
    #[error("connection failed: {0}")]
    Connection(String),

    #[error("authentication failed")]
    Authentication,

    #[error("query failed: {0}")]
    Query(String),

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
}
