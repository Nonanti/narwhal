//! Graphviz `dot` renderer using HTML record labels.
//!
//! Output is deterministic. Run `dot -Tsvg schema.dot -o schema.svg` to
//! turn it into an image — Graphviz is a runtime requirement of the
//! viewer, not of narwhal.

use std::fmt::Write as _;

use crate::model::{DiagramModel, Node, NodeColumn};
use crate::render::Renderer;

#[derive(Debug, Clone)]
pub struct DotRenderer {
    include_columns: bool,
    rankdir: &'static str,
}

impl Default for DotRenderer {
    fn default() -> Self {
        Self {
            include_columns: true,
            rankdir: "LR",
        }
    }
}

impl DotRenderer {
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub const fn without_columns(mut self) -> Self {
        self.include_columns = false;
        self
    }

    /// `LR` (default) or `TB`. Anything else is ignored.
    #[must_use]
    pub fn with_rankdir(mut self, rankdir: &str) -> Self {
        self.rankdir = match rankdir {
            "TB" | "tb" => "TB",
            "BT" | "bt" => "BT",
            "RL" | "rl" => "RL",
            _ => "LR",
        };
        self
    }
}

impl Renderer for DotRenderer {
    fn render(&self, model: &DiagramModel) -> String {
        let mut out = String::with_capacity(512);

        writeln!(&mut out, "digraph schema {{").ok();
        writeln!(&mut out, "  rankdir={};", self.rankdir).ok();
        writeln!(&mut out, "  graph [splines=ortho, nodesep=0.6, ranksep=0.8];").ok();
        writeln!(&mut out, "  node  [shape=plaintext, fontname=\"JetBrainsMono,Menlo,monospace\"];").ok();
        writeln!(&mut out, "  edge  [fontname=\"JetBrainsMono,Menlo,monospace\", fontsize=10];").ok();
        out.push('\n');

        for node in &model.nodes {
            write_node(&mut out, node, self.include_columns);
        }

        if !model.edges.is_empty() {
            out.push('\n');
        }
        for edge in &model.edges {
            let from_id = node_id(&edge.from.display());
            let to_id = node_id(&edge.to.display());
            // First column pair drives the port; composite FKs get a
            // joined edge label.
            let (from_port, to_port) = edge.columns.first().map_or_else(
                || (String::new(), String::new()),
                |(f, t)| (sanitize_port(f), sanitize_port(t)),
            );
            let from_ref = if from_port.is_empty() {
                from_id.clone()
            } else {
                format!("{from_id}:{from_port}")
            };
            let to_ref = if to_port.is_empty() {
                to_id.clone()
            } else {
                format!("{to_id}:{to_port}")
            };
            // Logical relations render dashed + grey so they read as
            // "informational, not engine-enforced" at a glance.
            let extra = if edge.kind.is_logical() {
                ", style=dashed, color=\"#888888\""
            } else {
                ""
            };
            writeln!(
                &mut out,
                "  {from_ref} -> {to_ref} [label=\"{label}\", arrowhead={head}{extra}];",
                label = escape_dq(&edge.label()),
                head = edge.cardinality.dot_arrowhead(),
            )
            .ok();
        }

        writeln!(&mut out, "}}").ok();
        out
    }
}

fn write_node(out: &mut String, node: &Node, include_columns: bool) {
    let id = node_id(&node.qualified.display());
    let title = html_escape(&node.qualified.display());
    write!(out, "  {id} [label=<<table border=\"0\" cellborder=\"1\" cellspacing=\"0\">").ok();
    write!(
        out,
        "<tr><td bgcolor=\"#cfe2ff\"><b>{title}</b></td></tr>"
    )
    .ok();
    if include_columns {
        for col in &node.columns {
            write!(
                out,
                "<tr><td port=\"{port}\" align=\"left\">{cell}</td></tr>",
                port = sanitize_port(&col.name),
                cell = html_escape(&render_column(col)),
            )
            .ok();
        }
    }
    writeln!(out, "</table>>];").ok();
}

fn render_column(col: &NodeColumn) -> String {
    let mut marker = String::new();
    if col.primary_key {
        marker.push_str("[PK] ");
    } else if col.foreign_key {
        marker.push_str("[FK] ");
    }
    if col.unique && !col.primary_key {
        marker.push_str("[UK] ");
    }
    let nullable = if col.nullable { "" } else { " *" };
    format!("{marker}{} : {}{nullable}", col.name, col.data_type)
}

/// DOT identifier rules: ASCII alphanumeric + `_`, must not start with a
/// digit.
fn node_id(qualified: &str) -> String {
    let mut s = String::with_capacity(qualified.len() + 1);
    for ch in qualified.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' {
            s.push(ch);
        } else {
            s.push('_');
        }
    }
    if s.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        s.insert(0, '_');
    }
    if s.is_empty() {
        s.push('_');
    }
    s
}

fn sanitize_port(name: &str) -> String {
    node_id(name)
}

fn escape_dq(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

fn html_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            other => out.push(other),
        }
    }
    out
}
