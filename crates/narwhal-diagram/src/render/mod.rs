//! String renderers for [`crate::DiagramModel`].
//!
//! The TUI widget consumes the model directly; only the export pipeline
//! and the MCP `get_diagram` tool go through these renderers.

pub mod dot;
pub mod mermaid;

pub use dot::DotRenderer;
pub use mermaid::MermaidRenderer;

use crate::model::DiagramModel;

/// Renderer contract: pure function from model to string.
pub trait Renderer {
    fn render(&self, model: &DiagramModel) -> String;
}
