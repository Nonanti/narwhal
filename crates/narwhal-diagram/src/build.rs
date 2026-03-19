//! Build a [`DiagramModel`] (and derived focused / impact views) from a
//! slice of [`TableSchema`].

use std::collections::{HashSet, VecDeque};

use narwhal_core::schema::{ForeignKey, TableSchema};
use serde::{Deserialize, Serialize};

use crate::model::{
    Cardinality, DiagramModel, Edge, EdgeKind, ImpactNode, ImpactTree, Node, NodeColumn,
    QualifiedName,
};

/// User-declared logical relation between two tables (FK-less join).
///
/// Hosts (`narwhal-config`, `.narwhal/workspace.toml` parser) build
/// these from TOML and pass them to [`build_with_logical`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogicalRelation {
    /// Child / referencing side. The diagram draws the edge
    /// `to → from` with `from` on the many side (matching FK semantics).
    pub from: QualifiedName,
    /// Parent / referenced side.
    pub to: QualifiedName,
    /// `(from_column, to_column)` pairs. V1 ships single-column
    /// relations; the field is plural so V1.1 can add composite support
    /// without changing the model wire format.
    pub columns: Vec<(String, String)>,
    /// Cardinality picked by the user (logical relations cannot
    /// derive it from FK metadata that does not exist).
    pub cardinality: Cardinality,
    /// Optional human note ("sharded across regions", "events stream").
    pub note: Option<String>,
}

/// Diagnostic emitted by [`build_with_logical`] when a logical
/// relation cannot be wired (unknown table or column on either side).
/// Hosts surface these in the status bar at start-up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BuildDiagnostic {
    UnknownTable {
        side: &'static str,
        table: QualifiedName,
    },
    UnknownColumn {
        table: QualifiedName,
        column: String,
    },
}

impl std::fmt::Display for BuildDiagnostic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownTable { side, table } => {
                write!(f, "logical relation: unknown {side} table `{}`", table.display())
            }
            Self::UnknownColumn { table, column } => {
                write!(
                    f,
                    "logical relation: column `{column}` not found in `{}`",
                    table.display()
                )
            }
        }
    }
}

/// Build the full diagram for `tables`.
///
/// Equivalent to [`build_with_logical`] with no logical relations.
/// Kept as the simple-case entry point so existing callers do not
/// have to plumb an empty slice.
pub fn build(tables: &[TableSchema]) -> DiagramModel {
    let (model, _diags) = build_with_logical(tables, &[]);
    model
}

/// Build the diagram including user-declared logical relations.
///
/// FKs whose referenced table is not in `tables` are dropped to keep
/// renderers from emitting dangling edges (same V1 rule as before).
/// Logical relations whose endpoints don't resolve get the same
/// treatment, but with a [`BuildDiagnostic`] returned in the second
/// tuple slot so the host can warn the user instead of silently
/// hiding them.
pub fn build_with_logical(
    tables: &[TableSchema],
    logical: &[LogicalRelation],
) -> (DiagramModel, Vec<BuildDiagnostic>) {
    let nodes: Vec<Node> = tables.iter().map(build_node).collect();

    let known: HashSet<QualifiedName> = nodes.iter().map(|n| n.qualified.clone()).collect();

    let mut edges = Vec::new();
    for table in tables {
        let from = QualifiedName::new(&table.table.schema, &table.table.name);
        for fk in &table.foreign_keys {
            let to = referenced_qualified(fk, &table.table.schema);
            if !known.contains(&to) {
                // Cross-schema or unknown target — skipped in V1.
                continue;
            }
            let cardinality = cardinality_for(table, fk);
            let columns: Vec<(String, String)> = fk
                .columns
                .iter()
                .cloned()
                .zip(fk.referenced_columns.iter().cloned())
                .collect();
            edges.push(Edge {
                kind: EdgeKind::ForeignKey,
                from: from.clone(),
                to,
                columns,
                cardinality,
                on_delete: fk.on_delete,
                on_update: fk.on_update,
                name: fk.name.clone(),
            });
        }
    }

    // Mark logical edges. Wire them after FKs so the diagnostic order
    // matches the user's declaration order and so a logical relation
    // that duplicates an existing FK is still appended (the user may
    // want to attach a note to it).
    let mut diagnostics = Vec::new();
    for (idx, rel) in logical.iter().enumerate() {
        // Unqualified relations (config without a schema prefix) are
        // resolved against the set of known tables by name when there
        // is exactly one match — mirrors how the `:diagram <table>`
        // command resolves bare names against the sidebar.
        let Some(from) = resolve_qualified(&nodes, &rel.from) else {
            diagnostics.push(BuildDiagnostic::UnknownTable {
                side: "from",
                table: rel.from.clone(),
            });
            continue;
        };
        let Some(to) = resolve_qualified(&nodes, &rel.to) else {
            diagnostics.push(BuildDiagnostic::UnknownTable {
                side: "to",
                table: rel.to.clone(),
            });
            continue;
        };
        // Validate column existence: surfaces typos at config load
        // time instead of at the first render.
        let from_node = node_by_name(&nodes, &from);
        let to_node = node_by_name(&nodes, &to);
        let mut column_ok = true;
        for (fc, tc) in &rel.columns {
            if let Some(node) = from_node {
                if !node.columns.iter().any(|c| &c.name == fc) {
                    diagnostics.push(BuildDiagnostic::UnknownColumn {
                        table: from.clone(),
                        column: fc.clone(),
                    });
                    column_ok = false;
                }
            }
            if let Some(node) = to_node {
                if !node.columns.iter().any(|c| &c.name == tc) {
                    diagnostics.push(BuildDiagnostic::UnknownColumn {
                        table: to.clone(),
                        column: tc.clone(),
                    });
                    column_ok = false;
                }
            }
        }
        if !column_ok {
            continue;
        }
        edges.push(Edge {
            kind: EdgeKind::Logical {
                note: rel.note.clone(),
            },
            from,
            to,
            columns: rel.columns.clone(),
            cardinality: rel.cardinality,
            on_delete: None,
            on_update: None,
            name: format!("logical_{idx}"),
        });
    }

    (DiagramModel { nodes, edges }, diagnostics)
}

fn node_by_name<'a>(nodes: &'a [Node], qn: &QualifiedName) -> Option<&'a Node> {
    nodes.iter().find(|n| &n.qualified == qn)
}

/// Resolve a possibly-unqualified `QualifiedName` against the cached
/// node list. Exact-schema match wins; otherwise, when the schema is
/// empty and exactly one node matches by name, return that node's
/// qualified name. Returns `None` for ambiguous or missing matches.
fn resolve_qualified(nodes: &[Node], qn: &QualifiedName) -> Option<QualifiedName> {
    if nodes.iter().any(|n| &n.qualified == qn) {
        return Some(qn.clone());
    }
    if !qn.schema.is_empty() {
        return None;
    }
    let mut matches = nodes.iter().filter(|n| n.qualified.name == qn.name);
    let first = matches.next()?;
    if matches.next().is_some() {
        // Ambiguous — multiple schemas have a table with this name.
        return None;
    }
    Some(first.qualified.clone())
}

/// Restrict `model` to `table` and every node reachable within `hops`
/// FK hops in either direction.
///
/// Hops are counted on the undirected FK graph: a 1-hop focus on `orders`
/// returns `users` (out-edge) **and** `order_items` (in-edge). Edges are
/// kept iff both endpoints survive the node filter.
pub fn focused(model: &DiagramModel, table: &QualifiedName, hops: usize) -> DiagramModel {
    if model.node(table).is_none() {
        return DiagramModel::default();
    }

    let mut keep: HashSet<QualifiedName> = HashSet::new();
    keep.insert(table.clone());

    let mut frontier: VecDeque<(QualifiedName, usize)> = VecDeque::new();
    frontier.push_back((table.clone(), 0));

    while let Some((current, depth)) = frontier.pop_front() {
        if depth >= hops {
            continue;
        }
        for edge in &model.edges {
            let neighbour = if edge.from == current {
                Some(&edge.to)
            } else if edge.to == current {
                Some(&edge.from)
            } else {
                None
            };
            if let Some(n) = neighbour {
                if keep.insert(n.clone()) {
                    frontier.push_back((n.clone(), depth + 1));
                }
            }
        }
    }

    let nodes: Vec<Node> = model
        .nodes
        .iter()
        .filter(|n| keep.contains(&n.qualified))
        .cloned()
        .collect();
    let edges: Vec<Edge> = model
        .edges
        .iter()
        .filter(|e| keep.contains(&e.from) && keep.contains(&e.to))
        .cloned()
        .collect();

    DiagramModel { nodes, edges }
}

/// Reverse-FK closure rooted at `table`.
///
/// At every level, lists the tables that have an FK pointing **at** the
/// current node. Cycles are broken by `visited`; a table appears at most
/// once in the entire tree.
pub fn impact(model: &DiagramModel, table: &QualifiedName) -> ImpactTree {
    // `visited` is seeded empty so a self-referential FK shows up once
    // (and only once) as an inbound child. Recursion blocks the second
    // visit because `edge.from` is already in `visited` after the first.
    let mut visited: HashSet<QualifiedName> = HashSet::new();
    let inbound = impact_children(model, table, &mut visited);
    ImpactTree {
        root: table.clone(),
        inbound,
    }
}

fn impact_children(
    model: &DiagramModel,
    target: &QualifiedName,
    visited: &mut HashSet<QualifiedName>,
) -> Vec<ImpactNode> {
    let mut out = Vec::new();
    for edge in &model.edges {
        if &edge.to != target {
            continue;
        }
        if !visited.insert(edge.from.clone()) {
            continue;
        }
        let children = impact_children(model, &edge.from, visited);
        out.push(ImpactNode {
            table: edge.from.clone(),
            fk_columns: edge.columns.iter().map(|(f, _)| f.clone()).collect(),
            on_delete: edge.on_delete,
            on_update: edge.on_update,
            children,
        });
    }
    out
}

fn build_node(table: &TableSchema) -> Node {
    // Collect single-column UNIQUE columns once so the per-column loop is O(c).
    let unique_singletons: HashSet<&str> = table
        .indexes
        .iter()
        .filter(|i| i.unique && !i.primary && i.columns.len() == 1)
        .filter_map(|i| i.columns.first().map(String::as_str))
        .chain(
            table
                .unique_constraints
                .iter()
                .filter(|u| u.columns.len() == 1)
                .filter_map(|u| u.columns.first().map(String::as_str)),
        )
        .collect();

    let fk_columns: HashSet<&str> = table
        .foreign_keys
        .iter()
        .flat_map(|fk| fk.columns.iter().map(String::as_str))
        .collect();

    let columns = table
        .columns
        .iter()
        .map(|c| NodeColumn {
            name: c.name.clone(),
            data_type: c.data_type.clone(),
            nullable: c.nullable,
            primary_key: c.primary_key,
            foreign_key: fk_columns.contains(c.name.as_str()),
            unique: unique_singletons.contains(c.name.as_str()),
        })
        .collect();

    Node {
        qualified: QualifiedName::new(&table.table.schema, &table.table.name),
        columns,
    }
}

fn referenced_qualified(fk: &ForeignKey, fallback_schema: &str) -> QualifiedName {
    let schema = fk
        .referenced_schema
        .as_deref()
        .unwrap_or(fallback_schema)
        .to_string();
    QualifiedName::new(schema, fk.referenced_table.clone())
}

fn cardinality_for(table: &TableSchema, fk: &ForeignKey) -> Cardinality {
    // A composite FK is "nullable" iff *any* of its columns is nullable —
    // the row can still link to a parent on the non-null part. Mermaid
    // does not model partial-null FKs so we collapse to nullable.
    let nullable = fk.columns.iter().any(|name| {
        // MSRV is 1.75 so `Option::is_none_or` (stable in 1.82) is not
        // available; the explicit match keeps clippy happy on both ends.
        match table.columns.iter().find(|c| &c.name == name) {
            Some(c) => c.nullable,
            None => true,
        }
    });

    // FK is "unique on the child side" iff a UNIQUE index / constraint
    // covers exactly the FK column set (order-insensitive). This collapses
    // many-to-many junction tables to two 1-to-many edges (correct) and
    // promotes 1-to-1 relationships to `||--||` (also correct).
    let unique = fk_is_unique(table, &fk.columns);

    match (nullable, unique) {
        (false, false) => Cardinality::OneToMany,
        (true, false) => Cardinality::ZeroOrOneToMany,
        (false, true) => Cardinality::OneToOne,
        (true, true) => Cardinality::ZeroOrOneToOne,
    }
}

fn fk_is_unique(table: &TableSchema, fk_columns: &[String]) -> bool {
    let fk_set: HashSet<&str> = fk_columns.iter().map(String::as_str).collect();
    // Primary key counts as unique.
    let pk_set: HashSet<&str> = table
        .columns
        .iter()
        .filter(|c| c.primary_key)
        .map(|c| c.name.as_str())
        .collect();
    if !pk_set.is_empty() && pk_set == fk_set {
        return true;
    }
    let index_match = table.indexes.iter().any(|i| {
        i.unique && i.columns.iter().map(String::as_str).collect::<HashSet<_>>() == fk_set
    });
    if index_match {
        return true;
    }
    table.unique_constraints.iter().any(|u| {
        u.columns.iter().map(String::as_str).collect::<HashSet<_>>() == fk_set
    })
}
