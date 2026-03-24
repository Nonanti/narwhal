use serde::{Deserialize, Serialize};

/// All visible schemas paired with the tables they own.
///
/// Return type of [`crate::Connection::list_all_tables`]. Lives at
/// the crate root so the dyn-safe sibling signatures don't trip
/// `clippy::type_complexity` on the nested `Vec<(Schema,
/// Vec<Table>)>`.
pub type SchemaCatalog = Vec<(Schema, Vec<Table>)>;

use crate::value::Value;

/// Logical schema or namespace inside a database.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Schema {
    pub name: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
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
    #[serde(default)]
    pub indexes: Vec<Index>,
    #[serde(default)]
    pub foreign_keys: Vec<ForeignKey>,
    #[serde(default)]
    pub unique_constraints: Vec<UniqueConstraint>,
}

/// Index defined on a table.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Index {
    pub name: String,
    pub columns: Vec<String>,
    pub unique: bool,
    /// `true` when the index is the implicit one created for the primary
    /// key. Useful when generating DDL, where the index is implied by the
    /// `PRIMARY KEY` declaration on the column instead.
    pub primary: bool,
}

/// Single foreign-key constraint.
///
/// Composite foreign keys are represented by parallel entries in
/// [`Self::columns`] and [`Self::referenced_columns`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ForeignKey {
    pub name: String,
    pub columns: Vec<String>,
    pub referenced_schema: Option<String>,
    pub referenced_table: String,
    pub referenced_columns: Vec<String>,
    pub on_update: Option<ReferentialAction>,
    pub on_delete: Option<ReferentialAction>,
}

/// Referential action declared on a foreign key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum ReferentialAction {
    NoAction,
    Restrict,
    Cascade,
    SetNull,
    SetDefault,
}

impl ReferentialAction {
    pub const fn as_sql(self) -> &'static str {
        match self {
            Self::NoAction => "NO ACTION",
            Self::Restrict => "RESTRICT",
            Self::Cascade => "CASCADE",
            Self::SetNull => "SET NULL",
            Self::SetDefault => "SET DEFAULT",
        }
    }

    pub fn from_engine_token(token: &str) -> Option<Self> {
        match token.trim().to_ascii_uppercase().as_str() {
            "NO ACTION" => Some(Self::NoAction),
            "RESTRICT" => Some(Self::Restrict),
            "CASCADE" => Some(Self::Cascade),
            "SET NULL" => Some(Self::SetNull),
            "SET DEFAULT" => Some(Self::SetDefault),
            _ => None,
        }
    }
}

/// Multi-column unique constraint.
///
/// Single-column unique constraints are exposed through [`Index::unique`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UniqueConstraint {
    pub name: String,
    pub columns: Vec<String>,
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
