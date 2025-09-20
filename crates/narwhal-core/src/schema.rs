use serde::{Deserialize, Serialize};

use crate::value::Value;

/// A logical schema/namespace inside a database (e.g. `public` for PostgreSQL).
/// For SQLite this is a synthetic single-entry list ("main").
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Schema {
    pub name: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TableKind {
    Table,
    View,
    MaterializedView,
    SystemTable,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Table {
    pub schema: String,
    pub name: String,
    pub kind: TableKind,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Column {
    pub name: String,
    /// Database-native type, e.g. `int4`, `text`, `varchar(255)`.
    pub data_type: String,
    pub nullable: bool,
    pub primary_key: bool,
    pub default: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TableSchema {
    pub table: Table,
    pub columns: Vec<Column>,
}

/// One row in a [`QueryResult`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Row(pub Vec<Value>);

impl Row {
    pub fn get(&self, idx: usize) -> Option<&Value> {
        self.0.get(idx)
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// Result of an executed query.
///
/// For DDL/DML without a result set, `columns` is empty and `rows_affected` is set.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct QueryResult {
    pub columns: Vec<ColumnHeader>,
    pub rows: Vec<Row>,
    pub rows_affected: Option<u64>,
    /// Total execution time in milliseconds (driver-reported, best-effort).
    pub elapsed_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ColumnHeader {
    pub name: String,
    pub data_type: String,
}

impl QueryResult {
    pub fn empty() -> Self {
        Self::default()
    }
}
