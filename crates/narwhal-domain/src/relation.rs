//! Domain types for logical relations between schema entities.
//!
//! These types represent the *conceptual* relationships between tables
//! (foreign-key–style edges, cardinality, qualified names). They live in
//! the domain crate because they are consumed by config parsing, diagram
//! building, and the TUI alike — they are not rendering output.
//!
//! `narwhal-diagram` re-exports them for backward compatibility so
//! existing consumers (`narwhal-tui`, `narwhal-mcp`, `narwhal-app`)
//! continue to compile without changes.

use serde::{Deserialize, Serialize};

/// Fully-qualified `schema.table` identifier.
///
/// Two qualified names are equal iff both their `schema` and `name` parts
/// are equal; this is the join key used by the diagram model when wiring
/// foreign keys to their referenced node.
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

/// User-declared logical relation between two tables (FK-less join).
///
/// Hosts (`narwhal-config`, `.narwhal/workspace.toml` parser) build
/// these from TOML and pass them to the diagram builder
/// (`narwhal_diagram::build_with_logical`).
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qualified_name_display_unqualified() {
        let qn = QualifiedName::new("", "users");
        assert_eq!(qn.display(), "users");
    }

    #[test]
    fn qualified_name_display_qualified() {
        let qn = QualifiedName::new("public", "users");
        assert_eq!(qn.display(), "public.users");
    }

    #[test]
    fn cardinality_parse_roundtrip() {
        for token in [
            "one-to-many",
            "zero-or-one-to-many",
            "one-to-one",
            "zero-or-one-to-one",
            "many-to-one",
            "many-to-many",
        ] {
            assert!(
                Cardinality::parse(token).is_some(),
                "failed to parse {token}"
            );
        }
    }

    #[test]
    fn cardinality_parse_aliases() {
        assert_eq!(
            Cardinality::parse("1-to-many"),
            Some(Cardinality::OneToMany)
        );
        assert_eq!(Cardinality::parse("1-to-1"), Some(Cardinality::OneToOne));
        assert_eq!(Cardinality::parse("n-to-1"), Some(Cardinality::ManyToOne));
        assert_eq!(Cardinality::parse("n-to-n"), Some(Cardinality::ManyToMany));
        assert_eq!(Cardinality::parse("m-to-n"), Some(Cardinality::ManyToMany));
        assert_eq!(
            Cardinality::parse("0..1-to-many"),
            Some(Cardinality::ZeroOrOneToMany)
        );
        assert_eq!(
            Cardinality::parse("0..1-to-1"),
            Some(Cardinality::ZeroOrOneToOne)
        );
    }

    #[test]
    fn cardinality_parse_unknown() {
        assert_eq!(Cardinality::parse("bogus"), None);
    }

    #[test]
    fn cardinality_mermaid_tokens() {
        assert_eq!(Cardinality::OneToMany.mermaid(), "||--o{");
        assert_eq!(Cardinality::ZeroOrOneToMany.mermaid(), "|o--o{");
        assert_eq!(Cardinality::OneToOne.mermaid(), "||--||");
        assert_eq!(Cardinality::ZeroOrOneToOne.mermaid(), "|o--o|");
        assert_eq!(Cardinality::ManyToOne.mermaid(), "}o--||");
        assert_eq!(Cardinality::ManyToMany.mermaid(), "}o--o{");
    }

    #[test]
    fn cardinality_dot_arrowhead() {
        assert_eq!(Cardinality::OneToMany.dot_arrowhead(), "crow");
        assert_eq!(Cardinality::OneToOne.dot_arrowhead(), "none");
        assert_eq!(Cardinality::ManyToMany.dot_arrowhead(), "crow");
    }
}
