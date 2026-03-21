//! Mermaid `erDiagram` renderer.
//!
//! Output is deterministic (nodes rendered in `model.nodes` order, edges
//! in `model.edges` order) so it is safe to commit to source control.

use std::fmt::Write as _;

use crate::model::{DiagramModel, Node, NodeColumn};
use crate::render::Renderer;

#[derive(Debug, Clone)]
pub struct MermaidRenderer {
    include_columns: bool,
    title: Option<String>,
}

impl Default for MermaidRenderer {
    fn default() -> Self {
        Self {
            include_columns: true,
            title: None,
        }
    }
}

impl MermaidRenderer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Render only table boxes, no column rows. Useful for big overview
    /// exports where columns clutter the picture.
    #[must_use]
    pub const fn without_columns(mut self) -> Self {
        self.include_columns = false;
        self
    }

    /// Set a Mermaid front-matter `title:` (rendered above the diagram by
    /// mermaid.live and the mermaid-cli).
    #[must_use]
    pub fn with_title(mut self, title: impl Into<String>) -> Self {
        self.title = Some(title.into());
        self
    }
}

impl Renderer for MermaidRenderer {
    fn render(&self, model: &DiagramModel) -> String {
        let mut out = String::with_capacity(256);

        if let Some(title) = &self.title {
            // YAML front-matter — Mermaid >= 10 supports this for titles.
            writeln!(&mut out, "---").ok();
            writeln!(&mut out, "title: {}", sanitize_title(title)).ok();
            writeln!(&mut out, "---").ok();
        }
        writeln!(&mut out, "erDiagram").ok();

        for node in &model.nodes {
            write_node(&mut out, node, self.include_columns);
        }

        if !model.edges.is_empty() {
            out.push('\n');
        }
        for edge in &model.edges {
            // Mermaid: parent (referenced) on the LEFT, child (referencing)
            // on the RIGHT. `edge.from` is the child in our model.
            // Logical (non-identifying) relations use `..` instead of
            // `--`; Mermaid renders them dashed and labels them as
            // non-identifying — exactly the semantics we want.
            let card = if edge.kind.is_logical() {
                edge.cardinality.mermaid().replace("--", "..")
            } else {
                edge.cardinality.mermaid().to_owned()
            };
            writeln!(
                &mut out,
                "    {parent} {card} {child} : \"{label}\"",
                parent = node_id(&edge.to.display()),
                child = node_id(&edge.from.display()),
                label = sanitize_label(&edge.label()),
            )
            .ok();
        }

        out
    }
}

fn write_node(out: &mut String, node: &Node, include_columns: bool) {
    let id = node_id(&node.qualified.display());
    if !include_columns || node.columns.is_empty() {
        writeln!(out, "    {id} {{ }}").ok();
        return;
    }
    writeln!(out, "    {id} {{").ok();
    for col in &node.columns {
        writeln!(out, "        {}", render_column(col)).ok();
    }
    writeln!(out, "    }}").ok();
}

fn render_column(col: &NodeColumn) -> String {
    let ty = sanitize_type(&col.data_type);
    let name = sanitize_ident(&col.name);
    let mut attrs: Vec<&str> = Vec::new();
    if col.primary_key {
        attrs.push("PK");
    }
    if col.foreign_key {
        attrs.push("FK");
    }
    if col.unique && !col.primary_key {
        attrs.push("UK");
    }
    if attrs.is_empty() {
        format!("{ty} {name}")
    } else {
        format!("{ty} {name} {}", attrs.join(","))
    }
}

/// Mermaid identifiers must match `[A-Za-z0-9_-]+`; we replace anything
/// else (including `.` for schema-qualified names) with `_`.
fn node_id(qualified: &str) -> String {
    let mut s = String::with_capacity(qualified.len());
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
    s
}

/// Mermaid attribute names: same rules as identifiers, conservatively.
fn sanitize_ident(name: &str) -> String {
    node_id(name)
}

/// Mermaid types must be a single token; collapse parens / commas / spaces.
fn sanitize_type(ty: &str) -> String {
    let trimmed = ty.trim();
    if trimmed.is_empty() {
        return "unknown".into();
    }
    let mut s = String::with_capacity(trimmed.len());
    for ch in trimmed.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' {
            s.push(ch);
        } else if !s.ends_with('_') {
            s.push('_');
        }
    }
    let trimmed = s.trim_matches('_');
    if trimmed.is_empty() {
        "unknown".into()
    } else {
        trimmed.into()
    }
}

/// Mermaid edge labels are quoted strings; a literal newline or tab
/// inside one breaks the parser ("unexpected token" error in
/// mermaid.live). Collapse whitespace control chars to a single space
/// and downgrade embedded double-quotes so the wrapping `"..."` still
/// delimits the label. Backslashes are *kept* — Mermaid has no
/// quoted-string escape vocabulary; a literal `\\` survives.
fn sanitize_label(label: &str) -> String {
    let mut out = String::with_capacity(label.len());
    for ch in label.chars() {
        match ch {
            '"' => out.push('\''),
            '\n' | '\r' | '\t' => out.push(' '),
            c if c.is_control() => out.push(' '),
            c => out.push(c),
        }
    }
    out
}

/// YAML front-matter title is delimited by `---` lines; strip newlines
/// (would close the front matter early) and the literal token `---`
/// (would terminate the block mid-title). The escape is conservative —
/// we replace, not reject — because titles are derived from connection
/// + table names, which are otherwise user-controlled.
fn sanitize_title(title: &str) -> String {
    title.replace(['\r', '\n'], " ").replace("---", "——")
}
