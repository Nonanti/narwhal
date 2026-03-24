//! Data model exposed by [`crate::build()`] and consumed by renderers and
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

/// Cardinality of an edge.
///
/// For foreign keys the variant is derived from column nullability +
/// uniqueness; for logical relations the user picks it explicitly
/// (nullable / unique are flaky concepts across shard / service
/// boundaries). Mermaid notation is given in parentheses
/// (parent on the left).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum Cardinality {
    /// FK NOT NULL, not UNIQUE  (`||--o{`).
    OneToMany,
    /// FK nullable, not UNIQUE  (`|o--o{`).
    ZeroOrOneToMany,
    /// FK NOT NULL, UNIQUE      (`||--||`).
    OneToOne,
    /// FK nullable, UNIQUE      (`|o--o|`).
    ZeroOrOneToOne,
    /// Logical-only: many on the child side, one on the parent side
    /// (`}o--||`). Use when the child relates to exactly one parent
    /// but the engine does not enforce it.
    ManyToOne,
    /// Logical-only: many on both sides (`}o--o{`). Junction tables
    /// already represented as two 1-to-many edges don't need this;
    /// it is for cross-boundary M:N where no junction table exists
    /// in this database.
    ManyToMany,
}

impl Cardinality {
    /// Mermaid `erDiagram` cardinality token (parent left, child right).
    pub const fn mermaid(self) -> &'static str {
        match self {
            Self::OneToMany => "||--o{",
            Self::ZeroOrOneToMany => "|o--o{",
            Self::OneToOne => "||--||",
            Self::ZeroOrOneToOne => "|o--o|",
            Self::ManyToOne => "}o--||",
            Self::ManyToMany => "}o--o{",
        }
    }

    /// Graphviz arrowhead style (`crow` = many, `none` = one).
    pub const fn dot_arrowhead(self) -> &'static str {
        match self {
            Self::OneToMany | Self::ZeroOrOneToMany | Self::ManyToOne | Self::ManyToMany => "crow",
            Self::OneToOne | Self::ZeroOrOneToOne => "none",
        }
    }

    /// Parse a kebab-case token used by config (`"one-to-many"`,
    /// `"many-to-one"`, ...). Returns `None` on unknown input so
    /// callers can surface a friendly error.
    pub fn parse(token: &str) -> Option<Self> {
        match token.trim().to_ascii_lowercase().replace('_', "-").as_str() {
            "one-to-many" | "1-to-many" => Some(Self::OneToMany),
            "zero-or-one-to-many" | "0..1-to-many" => Some(Self::ZeroOrOneToMany),
            "one-to-one" | "1-to-1" => Some(Self::OneToOne),
            "zero-or-one-to-one" | "0..1-to-1" => Some(Self::ZeroOrOneToOne),
            "many-to-one" | "n-to-1" => Some(Self::ManyToOne),
            "many-to-many" | "n-to-n" | "m-to-n" => Some(Self::ManyToMany),
            _ => None,
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
