//! narwhal-core — database-agnostic traits and types.
//!
//! New databases are added by implementing [`DatabaseDriver`] and [`Connection`]
//! in a separate crate. The rest of narwhal works against these traits only.

pub mod connection;
pub mod driver;
pub mod error;
pub mod schema;
pub mod value;

pub use connection::{Connection, ConnectionConfig, ConnectionParams};
pub use driver::DatabaseDriver;
pub use error::{Error, Result};
pub use schema::{Column, QueryResult, Row, Schema, Table, TableKind, TableSchema};
pub use value::Value;
