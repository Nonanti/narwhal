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

pub use build::{build, build_with_logical, focused, impact, BuildDiagnostic, LogicalRelation};
pub use icons::IconSet;
pub use model::{
    Cardinality, DiagramModel, Edge, EdgeKind, ImpactNode, ImpactTree, Node, NodeColumn,
    QualifiedName,
};
pub use render::{DotRenderer, MermaidRenderer, Renderer};
