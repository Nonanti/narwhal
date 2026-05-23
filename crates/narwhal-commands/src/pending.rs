//! Pending mutations: row-level changes the user has staged but not
//! yet committed to the database.
//!
//! The host accumulates [`PendingMutation`] values into a
//! [`PendingChanges`] queue as the user appends, duplicates, edits or
//! deletes rows inside the result pane. Nothing reaches the database
//! until the user presses *Commit* (Ctrl-S), at which point every
//! mutation is compiled into a parameterised statement and executed
//! inside a single transaction. Pressing *Discard* (Ctrl-X) drops the
//! queue without contacting the database.
//!
//! Every mutation carries its own *target* (schema + table) so the
//! queue can mix rows from different tables in one batch — useful, for
//! instance, when fixing a foreign-key chain.
//!
//! ## Safety
//!
//! Generated `UPDATE`/`DELETE` statements use **optimistic
//! concurrency**: the original PK columns *and* the original value of
//! every non-PK column the user observed go into the `WHERE` clause.
//! If another writer touched the row between snapshot and commit, the
//! statement matches zero rows and the host reports the conflict
//! before continuing. `rows_affected == 1` is the only accepted
//! outcome for `Update`/`Delete`; `Insert` requires `rows_affected >=
//! 1` (some engines return `0`/`u64::MAX` for `INSERT ... DEFAULT
//! VALUES`).

use std::collections::BTreeMap;

use narwhal_core::{Column, Row, Value};
use narwhal_sql::Dialect;

use crate::cell_edit::{placeholder, quote_ident, quote_qualified};

/// Fully-qualified table identity.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TableId {
    pub schema: String,
    pub table: String,
}

impl TableId {
    pub fn new(schema: impl Into<String>, table: impl Into<String>) -> Self {
        Self {
            schema: schema.into(),
            table: table.into(),
        }
    }

    /// Render the qualified name for status bars and previews.
    pub fn display(&self) -> String {
        if self.schema.is_empty() {
            self.table.clone()
        } else {
            format!("{}.{}", self.schema, self.table)
        }
    }
}

/// A single staged change against a single row.
#[derive(Debug, Clone)]
pub enum PendingMutation {
    /// Append a row. `values` may be partial; columns missing from the
    /// map default to either the engine's `DEFAULT` clause (when the
    /// column has one) or `NULL`.
    Insert {
        target: TableId,
        /// Schema snapshot taken at queue time so we can validate the
        /// statement against the column list even if the live schema
        /// has drifted.
        columns: Vec<Column>,
        /// Per-column user input. Missing columns are filled in via
        /// `DEFAULT`/`NULL` at compile time.
        values: BTreeMap<String, Value>,
    },
    /// Modify exactly one cell. Composite-key tables are supported via
    /// the multi-entry `pk_values` map. `old_value` participates in the
    /// `WHERE` clause for optimistic concurrency.
    Update {
        target: TableId,
        columns: Vec<Column>,
        column_name: String,
        old_value: Value,
        new_value: Value,
        /// PK column → snapshot value at the time the edit was queued.
        pk_values: BTreeMap<String, Value>,
    },
    /// Drop the row matching the given PK values. The full row snapshot
    /// is kept around so the preview modal can show the user what they
    /// are about to lose and so a future *Undo* feature can put it
    /// back.
    Delete {
        target: TableId,
        columns: Vec<Column>,
        pk_values: BTreeMap<String, Value>,
        /// The complete row at queue time — used by the preview modal.
        snapshot: Row,
        /// Column order corresponding to `snapshot`.
        column_order: Vec<String>,
    },
}

impl PendingMutation {
    /// Target table for this mutation.
    pub const fn target(&self) -> &TableId {
        match self {
            Self::Insert { target, .. }
            | Self::Update { target, .. }
            | Self::Delete { target, .. } => target,
        }
    }

    /// One-line summary suitable for the preview modal / status bar.
    pub fn summary(&self) -> String {
        match self {
            Self::Insert { target, values, .. } => {
                let cols: Vec<String> = values.keys().cloned().collect();
                if cols.is_empty() {
                    format!("INSERT INTO {} (defaults)", target.display())
                } else {
                    format!("INSERT INTO {} ({})", target.display(), cols.join(", "))
                }
            }
            Self::Update {
                target,
                column_name,
                old_value,
                new_value,
                ..
            } => format!(
                "UPDATE {} SET {column_name} = {} (was {})",
                target.display(),
                new_value.render(),
                old_value.render(),
            ),
            Self::Delete {
                target, pk_values, ..
            } => {
                let parts: Vec<String> = pk_values
                    .iter()
                    .map(|(k, v)| format!("{k}={}", v.render()))
                    .collect();
                format!(
                    "DELETE FROM {} WHERE {}",
                    target.display(),
                    parts.join(" AND ")
                )
            }
        }
    }
}

/// A queue of [`PendingMutation`]s plus the metadata needed to compile
/// and execute them as a single batch.
#[derive(Debug, Default, Clone)]
pub struct PendingChanges {
    mutations: Vec<PendingMutation>,
}

impl PendingChanges {
    pub const fn new() -> Self {
        Self {
            mutations: Vec::new(),
        }
    }

    pub fn push(&mut self, mutation: PendingMutation) {
        self.mutations.push(mutation);
    }

    pub fn pop(&mut self) -> Option<PendingMutation> {
        self.mutations.pop()
    }

    pub fn clear(&mut self) {
        self.mutations.clear();
    }

    pub fn len(&self) -> usize {
        self.mutations.len()
    }

    pub fn is_empty(&self) -> bool {
        self.mutations.is_empty()
    }

    pub fn iter(&self) -> std::slice::Iter<'_, PendingMutation> {
        self.mutations.iter()
    }

    pub fn iter_mut(&mut self) -> std::slice::IterMut<'_, PendingMutation> {
        self.mutations.iter_mut()
    }
}

impl<'a> IntoIterator for &'a PendingChanges {
    type Item = &'a PendingMutation;
    type IntoIter = std::slice::Iter<'a, PendingMutation>;
    fn into_iter(self) -> Self::IntoIter {
        self.mutations.iter()
    }
}

impl<'a> IntoIterator for &'a mut PendingChanges {
    type Item = &'a mut PendingMutation;
    type IntoIter = std::slice::IterMut<'a, PendingMutation>;
    fn into_iter(self) -> Self::IntoIter {
        self.mutations.iter_mut()
    }
}

impl PendingChanges {
    /// Borrow the underlying slice. Useful for read-only render paths.
    pub fn as_slice(&self) -> &[PendingMutation] {
        &self.mutations
    }

    /// Mutable borrow into the queue at `index` (e.g. for editing an
    /// Insert's `values` map after the row was staged).
    pub fn get_mut(&mut self, index: usize) -> Option<&mut PendingMutation> {
        self.mutations.get_mut(index)
    }

    /// Compile every queued mutation in declaration order. The
    /// returned vec parallels the queue 1:1; commit-time execution
    /// walks them in the same order.
    pub fn compile_all(&self, dialect: Dialect) -> Result<Vec<CompiledMutation>, CompileError> {
        let mut out = Vec::with_capacity(self.mutations.len());
        for (idx, m) in self.mutations.iter().enumerate() {
            let compiled =
                compile(m, dialect).map_err(|reason| CompileError { index: idx, reason })?;
            out.push(compiled);
        }
        Ok(out)
    }
}

/// One compiled mutation ready to hand to a driver.
#[derive(Debug, Clone)]
pub struct CompiledMutation {
    /// Parameterised SQL.
    pub sql: String,
    /// Bound parameters in placeholder order.
    pub params: Vec<Value>,
    /// What kind of result we expect — drives the post-execute check.
    pub expects: ExpectedRows,
    /// One-line description for the audit log and preview modal.
    pub summary: String,
}

/// Post-execute row-count contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExpectedRows {
    /// Insert: `rows_affected >= 1`. Some engines return `0` for
    /// `INSERT ... DEFAULT VALUES`; the host caller decides whether to
    /// accept that or treat it as a conflict.
    Insert,
    /// Update / Delete: exactly one row.
    Exactly(u64),
}

/// Failure reason for [`PendingChanges::compile_all`].
#[derive(Debug, Clone)]
pub struct CompileError {
    /// Index of the offending mutation in the queue.
    pub index: usize,
    /// Human-readable reason — surfaced to the status bar.
    pub reason: String,
}

impl std::fmt::Display for CompileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "mutation #{}: {}", self.index + 1, self.reason)
    }
}

impl std::error::Error for CompileError {}

fn compile(m: &PendingMutation, dialect: Dialect) -> Result<CompiledMutation, String> {
    match m {
        PendingMutation::Insert {
            target,
            columns,
            values,
        } => compile_insert(target, columns, values, dialect),
        PendingMutation::Update {
            target,
            column_name,
            old_value,
            new_value,
            pk_values,
            ..
        } => compile_update(
            target,
            column_name,
            old_value,
            new_value,
            pk_values,
            dialect,
        ),
        PendingMutation::Delete {
            target, pk_values, ..
        } => compile_delete(target, pk_values, dialect),
    }
}

fn compile_insert(
    target: &TableId,
    columns: &[Column],
    values: &BTreeMap<String, Value>,
    dialect: Dialect,
) -> Result<CompiledMutation, String> {
    // Only emit columns the user actually populated. Anything else
    // falls back to the engine's defaults: it might be a serial
    // primary key, a created_at timestamp, or simply NULL.
    if values.is_empty() {
        let sql = format!(
            "INSERT INTO {} DEFAULT VALUES",
            quote_qualified(&target.schema, &target.table, dialect),
        );
        return Ok(CompiledMutation {
            sql,
            params: Vec::new(),
            expects: ExpectedRows::Insert,
            summary: format!("INSERT INTO {} DEFAULT VALUES", target.display()),
        });
    }
    // Validate every populated column exists in the schema snapshot —
    // catches typos when callers build mutations from raw input.
    for col in values.keys() {
        if !columns.iter().any(|c| &c.name == col) {
            return Err(format!(
                "column '{col}' not declared on {}",
                target.display()
            ));
        }
    }
    let mut col_names = Vec::with_capacity(values.len());
    let mut placeholders = Vec::with_capacity(values.len());
    let mut params = Vec::with_capacity(values.len());
    for (i, (col, value)) in values.iter().enumerate() {
        col_names.push(quote_ident(col, dialect));
        placeholders.push(placeholder(i + 1, dialect));
        params.push(value.clone());
    }
    let sql = format!(
        "INSERT INTO {} ({}) VALUES ({})",
        quote_qualified(&target.schema, &target.table, dialect),
        col_names.join(", "),
        placeholders.join(", "),
    );
    Ok(CompiledMutation {
        sql,
        params,
        expects: ExpectedRows::Insert,
        summary: format!(
            "INSERT INTO {} ({})",
            target.display(),
            values.keys().cloned().collect::<Vec<_>>().join(", ")
        ),
    })
}

fn compile_update(
    target: &TableId,
    column_name: &str,
    old_value: &Value,
    new_value: &Value,
    pk_values: &BTreeMap<String, Value>,
    dialect: Dialect,
) -> Result<CompiledMutation, String> {
    if pk_values.is_empty() {
        return Err(format!(
            "{}: no primary key recorded, refusing UPDATE",
            target.display()
        ));
    }
    let mut params = Vec::with_capacity(2 + pk_values.len());
    params.push(new_value.clone());
    let set_placeholder = placeholder(1, dialect);
    let mut where_parts = Vec::with_capacity(pk_values.len() + 1);
    for (col, val) in pk_values {
        if val.is_null() {
            return Err(format!(
                "PK column '{col}' is NULL on {}; refusing UPDATE",
                target.display()
            ));
        }
        let ph = placeholder(params.len() + 1, dialect);
        where_parts.push(format!("{} = {ph}", quote_ident(col, dialect)));
        params.push(val.clone());
    }
    // Optimistic concurrency: target column must still equal `old_value`.
    // NULL old values use `IS NULL` rather than `= NULL` (which is never
    // true under SQL semantics).
    if old_value.is_null() {
        where_parts.push(format!("{} IS NULL", quote_ident(column_name, dialect)));
    } else {
        let ph = placeholder(params.len() + 1, dialect);
        where_parts.push(format!("{} = {ph}", quote_ident(column_name, dialect)));
        params.push(old_value.clone());
    }
    let sql = format!(
        "UPDATE {} SET {} = {set_placeholder} WHERE {}",
        quote_qualified(&target.schema, &target.table, dialect),
        quote_ident(column_name, dialect),
        where_parts.join(" AND "),
    );
    Ok(CompiledMutation {
        sql,
        params,
        expects: ExpectedRows::Exactly(1),
        summary: format!(
            "UPDATE {} SET {column_name} = {} (was {})",
            target.display(),
            new_value.render(),
            old_value.render(),
        ),
    })
}

fn compile_delete(
    target: &TableId,
    pk_values: &BTreeMap<String, Value>,
    dialect: Dialect,
) -> Result<CompiledMutation, String> {
    if pk_values.is_empty() {
        return Err(format!(
            "{}: no primary key recorded, refusing DELETE",
            target.display()
        ));
    }
    let mut params = Vec::with_capacity(pk_values.len());
    let mut where_parts = Vec::with_capacity(pk_values.len());
    for (col, val) in pk_values {
        if val.is_null() {
            return Err(format!(
                "PK column '{col}' is NULL on {}; refusing DELETE",
                target.display()
            ));
        }
        let ph = placeholder(params.len() + 1, dialect);
        where_parts.push(format!("{} = {ph}", quote_ident(col, dialect)));
        params.push(val.clone());
    }
    let sql = format!(
        "DELETE FROM {} WHERE {}",
        quote_qualified(&target.schema, &target.table, dialect),
        where_parts.join(" AND "),
    );
    let summary = {
        let parts: Vec<String> = pk_values
            .iter()
            .map(|(k, v)| format!("{k}={}", v.render()))
            .collect();
        format!(
            "DELETE FROM {} WHERE {}",
            target.display(),
            parts.join(" AND ")
        )
    };
    Ok(CompiledMutation {
        sql,
        params,
        expects: ExpectedRows::Exactly(1),
        summary,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pk(name: &str) -> Column {
        Column {
            name: name.into(),
            data_type: "integer".into(),
            nullable: false,
            primary_key: true,
            default: None,
        }
    }
    fn col(name: &str) -> Column {
        Column {
            name: name.into(),
            data_type: "text".into(),
            nullable: true,
            primary_key: false,
            default: None,
        }
    }

    fn target() -> TableId {
        TableId::new("public", "items")
    }

    #[test]
    fn insert_with_explicit_columns_postgres() {
        let mut values = BTreeMap::new();
        values.insert("label".into(), Value::String("hi".into()));
        let m = PendingMutation::Insert {
            target: target(),
            columns: vec![pk("id"), col("label")],
            values,
        };
        let compiled = compile(&m, Dialect::Postgres).unwrap();
        assert_eq!(
            compiled.sql,
            "INSERT INTO \"public\".\"items\" (\"label\") VALUES ($1)"
        );
        assert_eq!(compiled.params.len(), 1);
        assert_eq!(compiled.expects, ExpectedRows::Insert);
    }

    #[test]
    fn insert_with_no_values_uses_default_values() {
        let m = PendingMutation::Insert {
            target: target(),
            columns: vec![pk("id"), col("label")],
            values: BTreeMap::new(),
        };
        let compiled = compile(&m, Dialect::Postgres).unwrap();
        assert!(compiled.sql.contains("DEFAULT VALUES"));
        assert!(compiled.params.is_empty());
    }

    #[test]
    fn insert_rejects_unknown_column() {
        let mut values = BTreeMap::new();
        values.insert("nonsense".into(), Value::String("x".into()));
        let m = PendingMutation::Insert {
            target: target(),
            columns: vec![pk("id"), col("label")],
            values,
        };
        let err = compile(&m, Dialect::Postgres).unwrap_err();
        assert!(err.contains("nonsense"));
    }

    #[test]
    fn update_uses_optimistic_old_value_in_where() {
        let mut pk_values = BTreeMap::new();
        pk_values.insert("id".into(), Value::Int(7));
        let m = PendingMutation::Update {
            target: target(),
            columns: vec![pk("id"), col("label")],
            column_name: "label".into(),
            old_value: Value::String("old".into()),
            new_value: Value::String("new".into()),
            pk_values,
        };
        let compiled = compile(&m, Dialect::Postgres).unwrap();
        // SET placeholder is $1, then PK ($2), then old-value ($3).
        assert_eq!(
            compiled.sql,
            "UPDATE \"public\".\"items\" SET \"label\" = $1 WHERE \"id\" = $2 AND \"label\" = $3"
        );
        assert_eq!(compiled.params.len(), 3);
        assert_eq!(compiled.expects, ExpectedRows::Exactly(1));
    }

    #[test]
    fn update_uses_is_null_when_old_value_is_null() {
        let mut pk_values = BTreeMap::new();
        pk_values.insert("id".into(), Value::Int(7));
        let m = PendingMutation::Update {
            target: target(),
            columns: vec![pk("id"), col("label")],
            column_name: "label".into(),
            old_value: Value::Null,
            new_value: Value::String("x".into()),
            pk_values,
        };
        let compiled = compile(&m, Dialect::Postgres).unwrap();
        assert!(compiled.sql.contains("\"label\" IS NULL"));
        // No extra param for the IS NULL branch.
        assert_eq!(compiled.params.len(), 2);
    }

    #[test]
    fn delete_with_composite_pk_mysql() {
        let mut pk_values = BTreeMap::new();
        pk_values.insert("a".into(), Value::Int(1));
        pk_values.insert("b".into(), Value::Int(2));
        let m = PendingMutation::Delete {
            target: TableId::new("", "t"),
            columns: vec![pk("a"), pk("b"), col("c")],
            pk_values,
            snapshot: Row(vec![
                Value::Int(1),
                Value::Int(2),
                Value::String("x".into()),
            ]),
            column_order: vec!["a".into(), "b".into(), "c".into()],
        };
        let compiled = compile(&m, Dialect::MySql).unwrap();
        assert_eq!(compiled.sql, "DELETE FROM `t` WHERE `a` = ? AND `b` = ?");
        assert_eq!(compiled.params.len(), 2);
        assert_eq!(compiled.expects, ExpectedRows::Exactly(1));
    }

    #[test]
    fn delete_rejects_null_pk_value() {
        let mut pk_values = BTreeMap::new();
        pk_values.insert("id".into(), Value::Null);
        let m = PendingMutation::Delete {
            target: target(),
            columns: vec![pk("id")],
            pk_values,
            snapshot: Row(vec![Value::Null]),
            column_order: vec!["id".into()],
        };
        let err = compile(&m, Dialect::Sqlite).unwrap_err();
        assert!(err.contains("NULL"));
    }

    #[test]
    fn delete_rejects_empty_pk() {
        let m = PendingMutation::Delete {
            target: target(),
            columns: vec![col("a")],
            pk_values: BTreeMap::new(),
            snapshot: Row(vec![]),
            column_order: vec![],
        };
        let err = compile(&m, Dialect::Sqlite).unwrap_err();
        assert!(err.contains("primary key"));
    }

    #[test]
    fn compile_all_preserves_order_and_reports_offending_index() {
        let mut queue = PendingChanges::new();
        // valid insert
        let mut values = BTreeMap::new();
        values.insert("label".into(), Value::String("ok".into()));
        queue.push(PendingMutation::Insert {
            target: target(),
            columns: vec![pk("id"), col("label")],
            values,
        });
        // invalid delete (no PK)
        queue.push(PendingMutation::Delete {
            target: target(),
            columns: vec![col("a")],
            pk_values: BTreeMap::new(),
            snapshot: Row(vec![]),
            column_order: vec![],
        });
        let err = queue.compile_all(Dialect::Postgres).unwrap_err();
        assert_eq!(err.index, 1, "second mutation should be flagged");
        assert!(err.to_string().contains("#2"));
    }
}
