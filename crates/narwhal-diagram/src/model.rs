//! Data model exposed by [`crate::build`] and consumed by renderers and
//! the TUI widget.

use serde::{Deserialize, Serialize};

use narwhal_core::schema::ReferentialAction;

/// Fully-qualified `schema.table` identifier.
///
/// Two qualified names are equal iff both their `schema` and `name` parts
/// are equal; this is the join key used by the model when wiring foreign
/// keys to their referenced node.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct QualifiedName {
    pub schema: String,
    pub name: String,
}

impl QualifiedName {
    pub fn new(schema: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            schema: schema.into(),
            name: name.into(),
        }
    }

    /// `schema.name` for display and identifier sanitisation.
    pub fn display(&self) -> String {
        if self.schema.is_empty() {
            self.name.clone()
        } else {
            format!("{}.{}", self.schema, self.name)
        }
    }
}

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
///
/// V1 only emits [`Self::ForeignKey`]; the variant exists so that future
/// user-defined "logical" relations can be added without touching
/// renderers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum EdgeKind {
    ForeignKey,
}

/// Cardinality of a foreign-key relationship, computed from FK column
/// nullability and uniqueness.
///
/// Mermaid notation is given in parentheses (parent on the left).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Cardinality {
    /// FK NOT NULL, not UNIQUE  (`||--o{`).
    OneToMany,
    /// FK nullable, not UNIQUE  (`|o--o{`).
    ZeroOrOneToMany,
    /// FK NOT NULL, UNIQUE      (`||--||`).
    OneToOne,
    /// FK nullable, UNIQUE      (`|o--o|`).
    ZeroOrOneToOne,
}

impl Cardinality {
    /// Mermaid `erDiagram` cardinality token (parent left, child right).
    pub const fn mermaid(self) -> &'static str {
        match self {
            Self::OneToMany => "||--o{",
            Self::ZeroOrOneToMany => "|o--o{",
            Self::OneToOne => "||--||",
            Self::ZeroOrOneToOne => "|o--o|",
        }
    }

    /// Graphviz arrowhead style (`crow` = many, `none` = one).
    pub const fn dot_arrowhead(self) -> &'static str {
        match self {
            Self::OneToMany | Self::ZeroOrOneToMany => "crow",
            Self::OneToOne | Self::ZeroOrOneToOne => "none",
        }
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
    /// Short label used by renderers (joined FK column names).
    pub fn label(&self) -> String {
        self.columns
            .iter()
            .map(|(f, _)| f.as_str())
            .collect::<Vec<_>>()
            .join(",")
    }
}

/// Complete diagram: every node + every FK edge whose **referenced** side
/// resolves inside `nodes`.
///
/// FKs pointing at tables outside the slice (e.g. cross-schema in V1) are
/// dropped silently by [`crate::build`]; this is intentional so renderers
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
