//! DDL emission per dialect.
//!
//! The [`DdlEmitter`] trait is the contract every dialect implements.
//! Step 1 of T2-T2-C only ships the trait and the generic ANSI
//! fallback; Step 2 fills in postgres / mysql / sqlite / mssql.
//!
//! The trait is dyn-safe so the CLI can resolve a dialect by name at
//! runtime (`--dialect postgres`) without bringing every emitter into
//! the call site as a type parameter.

use crate::SchemaDiffError;
use crate::diff::SchemaDiff;

pub mod generic;
pub mod mssql;
pub mod mysql;
pub mod postgres;
pub mod sqlite;

pub use generic::GenericEmitter;
pub use mssql::MssqlEmitter;
pub use mysql::MysqlEmitter;
pub use postgres::PostgresEmitter;
pub use sqlite::SqliteEmitter;

/// One dialect's projection of a [`SchemaDiff`] into runnable DDL.
///
/// Implementations return one big `String` (the whole migration)
/// rather than streaming because the typical diff is small and the
/// TUI modal needs the full text up-front to render and yank. A
/// future streaming variant can be added without touching this trait.
pub trait DdlEmitter: Send + Sync {
    /// Stable identifier for the dialect, e.g. `"postgres"`. Used
    /// in error messages and as the value the CLI accepts for
    /// `--dialect`.
    fn name(&self) -> &'static str;

    /// Render `diff` as DDL. Returns an [`SchemaDiffError::Unsupported`]
    /// when the dialect cannot represent the requested change (e.g.
    /// `SQLite` + drop column on engines older than 3.35).
    ///
    /// # Errors
    ///
    /// Returns [`SchemaDiffError::Unsupported`] when the dialect
    /// cannot express a particular change; the diff entry that
    /// caused the failure is named in the error message.
    fn emit(&self, diff: &SchemaDiff) -> Result<String, SchemaDiffError>;
}

/// Resolve a dialect by name. Returns `None` for unknown names so
/// the CLI can surface "unknown dialect" instead of falling back
/// silently.
///
/// Recognised names (case-insensitive):
/// `generic` / `ansi` / `sql`, `postgres` / `postgresql` / `pg`,
/// `mysql` / `mariadb`, `sqlite`, `mssql` / `sqlserver`.
#[must_use]
pub fn emitter_by_name(name: &str) -> Option<Box<dyn DdlEmitter>> {
    match name.to_ascii_lowercase().as_str() {
        "generic" | "ansi" | "sql" => Some(Box::new(GenericEmitter::new())),
        "postgres" | "postgresql" | "pg" => Some(Box::new(PostgresEmitter::new())),
        "mysql" | "mariadb" => Some(Box::new(MysqlEmitter::new())),
        "sqlite" => Some(Box::new(SqliteEmitter::new())),
        "mssql" | "sqlserver" => Some(Box::new(MssqlEmitter::new())),
        _ => None,
    }
}

/// Render `"schema.name"`; drop the schema when empty so file-locals
/// (`SQLite`'s `"main"`) stay unqualified.
///
/// Shared by every dialect emitter so changing the qualification
/// policy (e.g. quoted identifiers in a future step) is a one-line
/// edit.
#[must_use]
pub(crate) fn qualify(schema: &str, name: &str) -> String {
    if schema.is_empty() {
        name.to_owned()
    } else {
        format!("{schema}.{name}")
    }
}
