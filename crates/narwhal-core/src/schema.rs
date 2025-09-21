use serde::{Deserialize, Serialize};

use crate::value::Value;

/// Logical schema or namespace inside a database.
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
    /// Native type name as reported by the engine (e.g. `int4`, `varchar(255)`).
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

/// Materialised result of an executed statement.
///
/// For non-`SELECT` statements `columns` and `rows` are empty and
/// `rows_affected` carries the engine-reported count when available.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct QueryResult {
    pub columns: Vec<ColumnHeader>,
    pub rows: Vec<Row>,
    pub rows_affected: Option<u64>,
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
