use thiserror::Error;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Error)]
pub enum Error {
    #[error("connection failed: {0}")]
    Connection(String),

    #[error("authentication failed")]
    Authentication,

    #[error("query failed: {0}")]
    Query(String),

    #[error("driver `{0}` not found")]
    UnknownDriver(String),

    #[error("unsupported value type: {0}")]
    UnsupportedType(String),

    #[error("schema error: {0}")]
    Schema(String),

    #[error("configuration error: {0}")]
    Config(String),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("{0}")]
    Other(String),
}

impl Error {
    pub fn other(msg: impl Into<String>) -> Self {
        Self::Other(msg.into())
    }
}
