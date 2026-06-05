//! Schema diff algorithm and result types.
//!
//! Inputs are two `&[TableSchema]` slices; the diff is computed by
//! sorting both sides on `(schema, table)` and walking them in
//! parallel. Per-table column / index / FK / unique diffs are
//! produced inline.
//!
//! Type and default comparisons go through
//! [`crate::normalise::canonical_type`] and
//! [`crate::normalise::defaults_equal`], so trivial format drift
//! (`varchar(255)` vs `character varying(255)`) is hidden.

use std::collections::{BTreeMap, BTreeSet};

use tracing::warn;

use narwhal_core::schema::{
    Column, ForeignKey, Index, ReferentialAction, Table, TableSchema, UniqueConstraint,
};
use serde::{Deserialize, Serialize};

use crate::normalise::{canonical_type, defaults_equal};

/// Outcome of comparing two schema slices.
///
/// Every nested collection is sorted by qualified name so successive
/// calls produce identical output; a CI gate can hash this struct and
/// fail on drift.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct SchemaDiff {
    /// Per-table changes ordered by `(schema, name)`.
    pub tables: Vec<TableChange>,
}

impl SchemaDiff {
    /// True when no changes were detected. Cheaper than checking
    /// `tables.is_empty()` only because it makes intent obvious at
    /// the call site.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.tables.is_empty()
    }

    /// Count of every individual change across every table — useful
    /// for status messages like "3 tables, 7 column changes".
    #[must_use]
    pub fn change_count(&self) -> usize {
        self.tables
            .iter()
            .map(|t| match t {
                TableChange::Added(_) | TableChange::Removed(_) => 1,
                TableChange::Changed {
                    columns,
                    indexes,
                    foreign_keys,
                    unique_constraints,
                    ..
                } => columns.len() + indexes.len() + foreign_keys.len() + unique_constraints.len(),
            })
            .sum()
    }
}

/// Change at the table level.
///
/// "Added" and "Removed" carry the full [`TableSchema`] because the
/// DDL emitter needs every field to render a `CREATE TABLE` /
/// `DROP TABLE`. "Changed" carries only the per-attribute diff plus
/// enough identity (`schema`, `name`) to qualify the `ALTER` target.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TableChange {
    /// Table exists in `source` but not `target` — must be created.
    Added(TableSchema),
    /// Table exists in `target` but not `source` — must be dropped.
    Removed(TableSchema),
    /// Table exists on both sides with at least one per-attribute
    /// difference.
    Changed {
        /// Qualified name of the table being altered.
        table: Table,
        /// Column-level deltas.
        columns: Vec<ColumnChange>,
        /// Index-level deltas (excluding the implicit primary-key index).
        indexes: Vec<IndexChange>,
        /// Foreign-key deltas.
        foreign_keys: Vec<ForeignKeyChange>,
        /// Multi-column unique-constraint deltas.
        unique_constraints: Vec<UniqueConstraintChange>,
    },
}

impl TableChange {
    /// Qualified-name accessor that abstracts over the three variants.
    /// Used by the TUI tree view for sorting and label rendering.
    #[must_use]
    pub fn qualified_name(&self) -> (&str, &str) {
        match self {
            Self::Added(t) | Self::Removed(t) => (t.table.schema.as_str(), t.table.name.as_str()),
            Self::Changed { table, .. } => (table.schema.as_str(), table.name.as_str()),
        }
    }
}

/// One column-level delta inside a `TableChange::Changed`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ColumnChange {
    /// Column exists in `source` but not `target`.
    Added(Column),
    /// Column exists in `target` but not `source`.
    Removed(Column),
    /// Column's `data_type` differs after canonicalisation.
    TypeChanged {
        /// Column name (unqualified).
        name: String,
        /// Source-side raw type, preserved for the DDL emitter.
        source: String,
        /// Target-side raw type.
        target: String,
    },
    /// Column's `nullable` flag differs.
    NullableChanged {
        /// Column name.
        name: String,
        /// Source-side nullability (the desired state).
        source: bool,
        /// Target-side nullability.
        target: bool,
    },
    /// Column's `default` differs after canonicalisation.
    DefaultChanged {
        /// Column name.
        name: String,
        /// Source-side default expression, verbatim.
        source: Option<String>,
        /// Target-side default expression, verbatim.
        target: Option<String>,
    },
}

impl ColumnChange {
    /// Column name accessor — every variant carries one.
    #[must_use]
    pub fn name(&self) -> &str {
        match self {
            Self::Added(c) | Self::Removed(c) => c.name.as_str(),
            Self::TypeChanged { name, .. }
            | Self::NullableChanged { name, .. }
            | Self::DefaultChanged { name, .. } => name.as_str(),
        }
    }
}

/// One index-level delta.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum IndexChange {
    /// Index in `source` only.
    Added(Index),
    /// Index in `target` only.
    Removed(Index),
    /// Same name, different shape (columns, uniqueness).
    Changed {
        /// Source-side definition.
        source: Index,
        /// Target-side definition.
        target: Index,
    },
}

/// One foreign-key delta.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ForeignKeyChange {
    /// FK in `source` only.
    Added(ForeignKey),
    /// FK in `target` only.
    Removed(ForeignKey),
    /// Same name, different shape (columns, referenced table,
    /// referential actions).
    Changed {
        /// Source-side definition.
        source: ForeignKey,
        /// Target-side definition.
        target: ForeignKey,
    },
}

/// One unique-constraint delta.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum UniqueConstraintChange {
    /// Constraint in `source` only.
    Added(UniqueConstraint),
    /// Constraint in `target` only.
    Removed(UniqueConstraint),
    /// Same name, different column set.
    Changed {
        /// Source-side definition.
        source: UniqueConstraint,
        /// Target-side definition.
        target: UniqueConstraint,
    },
}

/// Compute the diff that would migrate `target` onto `source`.
///
/// The result is deterministic: every list is sorted by qualified
/// name. Empty inputs are valid and produce an empty `SchemaDiff`.
///
/// ## System schemas
///
/// Drivers leak `pg_catalog`, `information_schema`, `sys`,
/// `mysql.performance_schema`, etc. The diff filters those *exactly*
/// matching the well-known set so user tables that happen to share
/// a prefix (`pg_catalog_clone`) survive.
#[must_use]
pub fn diff(source: &[TableSchema], target: &[TableSchema]) -> SchemaDiff {
    let source_by_name = index_by_qualified_name(source);
    let target_by_name = index_by_qualified_name(target);

    let mut tables: Vec<TableChange> = Vec::new();

    // Single pass over the union of keys. BTreeSet iteration order
    // gives us sorted output for free.
    let mut all_keys: BTreeSet<(String, String)> = BTreeSet::new();
    all_keys.extend(source_by_name.keys().cloned());
    all_keys.extend(target_by_name.keys().cloned());

    for key in &all_keys {
        if is_system_schema(&key.0) {
            continue;
        }
        match (source_by_name.get(key), target_by_name.get(key)) {
            (Some(src), None) => tables.push(TableChange::Added((*src).clone())),
            (None, Some(tgt)) => tables.push(TableChange::Removed((*tgt).clone())),
            (Some(src), Some(tgt)) => {
                if let Some(change) = diff_one_table(src, tgt) {
                    tables.push(change);
                }
            }
            (None, None) => unreachable!("key originated from one of the two maps"),
        }
    }

    SchemaDiff { tables }
}

fn index_by_qualified_name(schemas: &[TableSchema]) -> BTreeMap<(String, String), &TableSchema> {
    schemas
        .iter()
        .map(|t| ((t.table.schema.clone(), t.table.name.clone()), t))
        .collect()
}

fn is_system_schema(schema: &str) -> bool {
    // Exact-match. Substring matching would catch user tables that
    // legitimately have a `pg_`-style prefix.
    matches!(
        schema.to_ascii_lowercase().as_str(),
        "pg_catalog"
            | "information_schema"
            | "pg_toast"
            | "sys"
            | "mysql"
            | "performance_schema"
            | "sys_schema"
    )
}

fn diff_one_table(source: &TableSchema, target: &TableSchema) -> Option<TableChange> {
    let columns = diff_columns(&source.columns, &target.columns);
    let indexes = diff_indexes(&source.indexes, &target.indexes);
    let foreign_keys = diff_foreign_keys(&source.foreign_keys, &target.foreign_keys);
    let unique_constraints = diff_unique(&source.unique_constraints, &target.unique_constraints);

    if columns.is_empty()
        && indexes.is_empty()
        && foreign_keys.is_empty()
        && unique_constraints.is_empty()
    {
        return None;
    }

    Some(TableChange::Changed {
        // We adopt the source-side `Table` identity so downstream
        // emitters render the desired post-migration `kind`. The
        // schema and name are equal by construction (the key match).
        table: source.table.clone(),
        columns,
        indexes,
        foreign_keys,
        unique_constraints,
    })
}

fn diff_columns(source: &[Column], target: &[Column]) -> Vec<ColumnChange> {
    // R3-M2: MR-M4 originally only routed FK / unique / index
    // lookups through `index_by_lower_name`; columns kept the old
    // silent-overwrite `.collect::<BTreeMap>()`. That left a
    // quoted-identifier hole in Postgres (`"Email"` vs `"email"`
    // are distinct columns) and a `COLLATE`-sensitive hole in
    // MSSQL. Route through the same helper so collisions emit
    // `tracing::warn` for diagnostic visibility.
    let source_by_name = index_by_lower_name(source.iter(), |c| &c.name, "column");
    let target_by_name = index_by_lower_name(target.iter(), |c| &c.name, "column");

    let mut keys: BTreeSet<String> = BTreeSet::new();
    keys.extend(source_by_name.keys().cloned());
    keys.extend(target_by_name.keys().cloned());

    let mut out: Vec<ColumnChange> = Vec::new();
    for key in &keys {
        match (source_by_name.get(key), target_by_name.get(key)) {
            (Some(src), None) => out.push(ColumnChange::Added((*src).clone())),
            (None, Some(tgt)) => out.push(ColumnChange::Removed((*tgt).clone())),
            (Some(src), Some(tgt)) => {
                if canonical_type(&src.data_type) != canonical_type(&tgt.data_type) {
                    out.push(ColumnChange::TypeChanged {
                        name: src.name.clone(),
                        source: src.data_type.clone(),
                        target: tgt.data_type.clone(),
                    });
                }
                if src.nullable != tgt.nullable {
                    out.push(ColumnChange::NullableChanged {
                        name: src.name.clone(),
                        source: src.nullable,
                        target: tgt.nullable,
                    });
                }
                if !defaults_equal(src.default.as_deref(), tgt.default.as_deref()) {
                    out.push(ColumnChange::DefaultChanged {
                        name: src.name.clone(),
                        source: src.default.clone(),
                        target: tgt.default.clone(),
                    });
                }
            }
            (None, None) => unreachable!(),
        }
    }
    out
}

fn diff_indexes(source: &[Index], target: &[Index]) -> Vec<IndexChange> {
    // Indexes are identified by name; we exclude the implicit primary
    // -key index because the diff for that lives on the column /
    // table-creation level. Review fix N1 / MR-M4: case-insensitive
    // index with warn-on-collision.
    let source_by_name =
        index_by_lower_name(source.iter().filter(|i| !i.primary), |i| &i.name, "index");
    let target_by_name =
        index_by_lower_name(target.iter().filter(|i| !i.primary), |i| &i.name, "index");

    let mut keys: BTreeSet<String> = BTreeSet::new();
    keys.extend(source_by_name.keys().cloned());
    keys.extend(target_by_name.keys().cloned());

    let mut out: Vec<IndexChange> = Vec::new();
    for key in &keys {
        match (source_by_name.get(key), target_by_name.get(key)) {
            (Some(src), None) => out.push(IndexChange::Added((*src).clone())),
            (None, Some(tgt)) => out.push(IndexChange::Removed((*tgt).clone())),
            (Some(src), Some(tgt)) => {
                if indexes_differ(src, tgt) {
                    out.push(IndexChange::Changed {
                        source: (*src).clone(),
                        target: (*tgt).clone(),
                    });
                }
            }
            (None, None) => unreachable!(),
        }
    }
    out
}

fn indexes_differ(left: &Index, right: &Index) -> bool {
    // Column order *does* matter for indexes — `(a, b)` and `(b, a)`
    // are different B-tree structures with different selectivity.
    left.columns != right.columns || left.unique != right.unique
}

fn diff_foreign_keys(source: &[ForeignKey], target: &[ForeignKey]) -> Vec<ForeignKeyChange> {
    // Review fix N1 / MR-M4: case-insensitive FK index. Cross-
    // dialect diffs (Postgres lower-cased vs MSSQL case-preserved)
    // used to surface phantom Add/Remove pairs for the same logical
    // constraint. `index_by_lower_name` emits a `tracing::warn` on
    // case-only collisions so silent overwrites can't hide a real
    // duplicate-name bug in the source catalog.
    let source_by_name = index_by_lower_name(source.iter(), |f| &f.name, "foreign key");
    let target_by_name = index_by_lower_name(target.iter(), |f| &f.name, "foreign key");

    let mut keys: BTreeSet<String> = BTreeSet::new();
    keys.extend(source_by_name.keys().cloned());
    keys.extend(target_by_name.keys().cloned());

    let mut out: Vec<ForeignKeyChange> = Vec::new();
    for key in &keys {
        match (source_by_name.get(key), target_by_name.get(key)) {
            (Some(src), None) => out.push(ForeignKeyChange::Added((*src).clone())),
            (None, Some(tgt)) => out.push(ForeignKeyChange::Removed((*tgt).clone())),
            (Some(src), Some(tgt)) => {
                if foreign_keys_differ(src, tgt) {
                    out.push(ForeignKeyChange::Changed {
                        source: (*src).clone(),
                        target: (*tgt).clone(),
                    });
                }
            }
            (None, None) => unreachable!(),
        }
    }
    out
}

fn foreign_keys_differ(left: &ForeignKey, right: &ForeignKey) -> bool {
    left.columns != right.columns
        || left.referenced_schema != right.referenced_schema
        || left.referenced_table != right.referenced_table
        || left.referenced_columns != right.referenced_columns
        || referential_action_differs(left.on_update, right.on_update)
        || referential_action_differs(left.on_delete, right.on_delete)
}

/// `NoAction` is the SQL default; treat `None` and `Some(NoAction)`
/// as equal so a driver that materialises the default doesn't
/// produce phantom diffs against one that omits it.
fn referential_action_differs(
    left: Option<ReferentialAction>,
    right: Option<ReferentialAction>,
) -> bool {
    let l = left.unwrap_or(ReferentialAction::NoAction);
    let r = right.unwrap_or(ReferentialAction::NoAction);
    l != r
}

fn diff_unique(
    source: &[UniqueConstraint],
    target: &[UniqueConstraint],
) -> Vec<UniqueConstraintChange> {
    // Review fix N1 / MR-M4: case-insensitive index, same warning
    // semantics as the FK diff.
    let source_by_name = index_by_lower_name(source.iter(), |u| &u.name, "unique constraint");
    let target_by_name = index_by_lower_name(target.iter(), |u| &u.name, "unique constraint");

    let mut keys: BTreeSet<String> = BTreeSet::new();
    keys.extend(source_by_name.keys().cloned());
    keys.extend(target_by_name.keys().cloned());

    let mut out: Vec<UniqueConstraintChange> = Vec::new();
    for key in &keys {
        match (source_by_name.get(key), target_by_name.get(key)) {
            (Some(src), None) => out.push(UniqueConstraintChange::Added((*src).clone())),
            (None, Some(tgt)) => out.push(UniqueConstraintChange::Removed((*tgt).clone())),
            (Some(src), Some(tgt)) => {
                if src.columns != tgt.columns {
                    out.push(UniqueConstraintChange::Changed {
                        source: (*src).clone(),
                        target: (*tgt).clone(),
                    });
                }
            }
            (None, None) => unreachable!(),
        }
    }
    out
}

/// MR-M4: build a case-insensitive lookup over `items` keyed by the
/// lower-cased name returned from `name_of`. Two items whose names
/// differ only in case both land on the same key; the first one
/// wins and the conflict is reported via `tracing::warn` so silent
/// overwrites don't hide an upstream catalog bug.
fn index_by_lower_name<'a, T, I, F>(
    items: I,
    name_of: F,
    kind: &'static str,
) -> BTreeMap<String, &'a T>
where
    T: 'a,
    I: IntoIterator<Item = &'a T>,
    F: Fn(&T) -> &str,
{
    let mut map: BTreeMap<String, &'a T> = BTreeMap::new();
    for item in items {
        let original = name_of(item).to_owned();
        let key = original.to_ascii_lowercase();
        if let Some(prev) = map.insert(key.clone(), item) {
            let prev_name = name_of(prev).to_owned();
            // Restore the original entry; "first-wins" is a deliberate
            // policy choice so callers get a deterministic result.
            map.insert(key.clone(), prev);
            warn!(
                target: "narwhal::schema_diff",
                kind, key, prev_name, dropped_name = original,
                "case-only {kind} name collision; keeping first occurrence"
            );
        }
    }
    map
}
