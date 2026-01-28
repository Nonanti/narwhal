//! Database-agnostic abstractions shared across the narwhal workspace.
//!
//! Drivers implement [`DatabaseDriver`] and [`Connection`]; the rest of the
//! application interacts with trait objects and is unaware of the underlying
//! database engine.

#![forbid(unsafe_code)]

pub mod cancel;
pub mod capabilities;
pub mod connection;
pub mod driver;
pub mod error;
pub mod schema;
pub mod ssh;
pub mod stream;
pub mod value;

pub use cancel::CancelHandle;
pub use capabilities::Capabilities;
pub use connection::{
    Connection, ConnectionConfig, ConnectionParams, IsolationLevel, SshConfig, SslMode,
};
pub use driver::DatabaseDriver;
pub use error::{Error, Result};
pub use schema::{
    Column, ColumnHeader, ForeignKey, Index, QueryResult, ReferentialAction, Row, Schema, Table,
    TableKind, TableSchema, UniqueConstraint,
};
pub use ssh::{SshTunnel, READY_TIMEOUT as SSH_READY_TIMEOUT};
pub use stream::RowStream;
pub use value::Value;
