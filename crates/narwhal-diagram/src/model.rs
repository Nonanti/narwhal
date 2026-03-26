//! Data model exposed by [`crate::build()`] and consumed by renderers and
//! the TUI widget.

use serde::{Deserialize, Serialize};

use narwhal_core::schema::ReferentialAction;

// Re-export from narwhal-domain so downstream consumers can still use
// `narwhal_diagram::{QualifiedName, Cardinality}`.
pub use narwhal_domain::{Cardinality, QualifiedName};

/// One column of a [`Node`], enriched with constraint flags so renderers
/// do not need to walk the original `TableSchema` again.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeColumn {
    pub name: String,
    pub data_type: String,
    pub nullable: bool,
    pub primary_key: bool,
    pub foreign_key: bool,
    /// `true` when the column participates in a single-column UNIQUE
    /// index or constraint (other than the PK).
    pub unique: bool,
}

/// One table in the diagram.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Node {
    pub qualified: QualifiedName,
    pub columns: Vec<NodeColumn>,
}

/// Kind of relationship between two nodes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum EdgeKind {
    /// Foreign-key constraint reported by the engine.
    ForeignKey,
    /// User-defined logical relation declared in config (FK-less join
    /// across micro-service / shard boundaries). Carries an optional
    /// human note shown in the TUI Impact view.
    Logical { note: Option<String> },
}

impl EdgeKind {
    /// `true` for [`Self::Logical`]; renderers branch on this to pick
    /// the dashed-line style.
    pub const fn is_logical(&self) -> bool {
        matches!(self, Self::Logical { .. })
    }
}

/// One foreign-key relationship: `from` (child / referencing) → `to`
/// (parent / referenced).
///
/// Composite foreign keys are kept as a single edge with parallel entries
/// in `columns`; this matches how `narwhal_core::ForeignKey` represents
/// them.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Edge {
    pub kind: EdgeKind,
    /// Child / referencing side.
    pub from: QualifiedName,
    /// Parent / referenced side.
    pub to: QualifiedName,
    /// `(from_column, to_column)` pairs.
    pub columns: Vec<(String, String)>,
    pub cardinality: Cardinality,
    pub on_delete: Option<ReferentialAction>,
    pub on_update: Option<ReferentialAction>,
    /// Original constraint name from the database.
    pub name: String,
}

impl Edge {
    /// Short label used by renderers (joined column names, with a
    /// `[logical]` suffix when the edge is user-defined).
    pub fn label(&self) -> String {
        let base = self
            .columns
            .iter()
            .map(|(f, _)| f.as_str())
            .collect::<Vec<_>>()
            .join(",");
        if self.kind.is_logical() {
            format!("{base} [logical]")
        } else {
            base
        }
    }
}

/// Complete diagram: every node + every FK edge whose **referenced** side
/// resolves inside `nodes`.
///
/// FKs pointing at tables outside the slice (e.g. cross-schema in V1) are
/// dropped silently by [`crate::build()`]; this is intentional so renderers
/// never emit dangling edges.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DiagramModel {
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
}

impl DiagramModel {
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// Find a node by qualified name (linear scan; diagrams have at most
    /// a few hundred nodes).
    pub fn node(&self, qn: &QualifiedName) -> Option<&Node> {
        self.nodes.iter().find(|n| &n.qualified == qn)
    }
}

/// One level of an [`ImpactTree`]: a table that references the parent,
/// plus the tables that reference *it* recursively.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImpactNode {
    pub table: QualifiedName,
    pub fk_columns: Vec<String>,
    pub on_delete: Option<ReferentialAction>,
    pub on_update: Option<ReferentialAction>,
    pub children: Vec<Self>,
}

/// Reverse-FK closure rooted at a single table.
///
/// Useful for "what breaks if I drop / rename this?" questions. Cycles
/// are broken by tracking visited tables during traversal.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImpactTree {
    pub root: QualifiedName,
    pub inbound: Vec<ImpactNode>,
}

impl ImpactTree {
    pub fn is_empty(&self) -> bool {
        self.inbound.is_empty()
    }
}
