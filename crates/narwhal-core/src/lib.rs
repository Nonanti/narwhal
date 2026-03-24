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
pub mod future;
pub mod query_stream;
pub mod schema;
pub mod ssh;
pub mod stream;
pub mod value;

pub use future::BoxFuture;

pub use cancel::{CancelHandle, DynCancelHandle};
pub use capabilities::Capabilities;
pub use connection::{
    Connection, ConnectionColor, ConnectionConfig, ConnectionParams, DynConnection, IsolationLevel,
    PreConnectStep, SshConfig, SslMode,
};
pub use driver::{DatabaseDriver, DynDatabaseDriver};
pub use error::{Error, Result};
pub use query_stream::QueryStream;
pub use schema::{
    Column, ColumnHeader, ForeignKey, Index, QueryResult, ReferentialAction, Row, Schema,
    SchemaCatalog, Table, TableKind, TableSchema, UniqueConstraint,
};
pub use ssh::{READY_TIMEOUT as SSH_READY_TIMEOUT, SshTunnel};
pub use stream::{DynRowStream, RowStream};
pub use value::Value;
