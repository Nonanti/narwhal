//! Headless schema-diagram model and renderers for narwhal.
//!
//! Given a slice of [`narwhal_core::TableSchema`], this crate produces a
//! [`DiagramModel`] (nodes + foreign-key edges with cardinality), a
//! reverse-FK [`ImpactTree`], and string renderers for Mermaid
//! `erDiagram` and Graphviz `dot`.
//!
//! The crate is intentionally TUI-free: a separate widget in
//! `narwhal-tui` consumes [`DiagramModel`] directly without going through
//! a string format.

#![forbid(unsafe_code)]

pub mod build;
pub mod icons;
pub mod model;
pub mod render;

pub use build::{BuildDiagnostic, LogicalRelation, build, build_with_logical, focused, impact};
pub use icons::IconSet;
pub use model::{
    Cardinality, DiagramModel, Edge, EdgeKind, ImpactNode, ImpactTree, Node, NodeColumn,
    QualifiedName,
};
pub use render::{DotRenderer, MermaidRenderer, Renderer};

// Backward-compat re-exports: these types now live in narwhal-domain.
// The `pub use` above (via model/build) already re-exports them transitively;
// this comment documents the migration for future maintainers.
// LogicalRelation, Cardinality, QualifiedName were moved from this crate
// to narwhal-domain in the v2.0 C4 refactor.
