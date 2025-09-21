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
pub mod value;

pub use cancel::CancelHandle;
pub use capabilities::Capabilities;
pub use connection::{Connection, ConnectionConfig, ConnectionParams, IsolationLevel};
pub use driver::DatabaseDriver;
pub use error::{Error, Result};
pub use schema::{Column, ColumnHeader, QueryResult, Row, Schema, Table, TableKind, TableSchema};
pub use value::Value;
