# narwhal-diagram

Headless schema-diagram model and renderers used by the narwhal TUI,
CLI export pipeline and MCP server.

This crate has **no TUI dependencies**. Given a slice of
`narwhal_core::TableSchema`, it produces:

- a `DiagramModel` (nodes + foreign-key edges with cardinality)
- a `MermaidRenderer` output (`erDiagram` syntax)
- a `DotRenderer` output (Graphviz `digraph` with HTML record labels)
- focused sub-models (`focused`) and reverse-FK impact trees (`impact`)

Renderers are pure functions of the model; the TUI consumes the model
directly without going through a string format.

```rust
use narwhal_diagram::{build, MermaidRenderer, Renderer};

let model = build(&tables);
let mermaid = MermaidRenderer::new().render(&model);
println!("{mermaid}");
```
