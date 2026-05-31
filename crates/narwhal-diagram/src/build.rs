//! Build a [`DiagramModel`] (and derived focused / impact views) from a
//! slice of [`TableSchema`].

use std::collections::{HashSet, VecDeque};

use narwhal_core::schema::{ForeignKey, TableSchema};

use crate::model::{
    Cardinality, DiagramModel, Edge, EdgeKind, ImpactNode, ImpactTree, Node, NodeColumn,
    QualifiedName,
};

/// Build the full diagram for `tables`.
///
/// FKs whose referenced table is not in `tables` are dropped — this keeps
/// renderers from emitting dangling edges. The referenced schema falls
/// back to the referencing table's schema when `ForeignKey::referenced_schema`
/// is `None` (the common case for same-schema FKs).
pub fn build(tables: &[TableSchema]) -> DiagramModel {
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

    DiagramModel { nodes, edges }
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
