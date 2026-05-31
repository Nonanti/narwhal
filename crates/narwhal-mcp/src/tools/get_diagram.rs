//! `get_diagram` — render an ER diagram of a connection's schema as
//! Mermaid (`erDiagram`) or Graphviz (`dot`) source.
//!
//! Mirrors the in-TUI `:diagram export` command: when `table` is given
//! the output is the focused 1-hop subset, otherwise it is the full
//! schema (or a single schema if `schema` is set).
//!
//! Agents typically use this after `describe_schema` to get a
//! human-readable bird's-eye view of FK relationships without having
//! to walk the JSON tree themselves.

use async_trait::async_trait;
use serde_json::{json, Value};

use narwhal_config::collect_logical_relations_for;
use narwhal_diagram::{
    build_with_logical, focused as diagram_focused, DotRenderer, MermaidRenderer, QualifiedName,
    Renderer,
};

use crate::context::ServerContext;
use crate::error::McpError;
use crate::tools::{cap_response, Tool, ToolOutput};

pub struct GetDiagramTool;

/// Output format the agent requested.
#[derive(Debug, Clone, Copy)]
enum DiagramFormat {
    Mermaid,
    Dot,
}

impl DiagramFormat {
    fn parse(token: &str) -> Option<Self> {
        match token.to_ascii_lowercase().as_str() {
            "mermaid" | "mmd" | "mer" => Some(Self::Mermaid),
            "dot" | "gv" | "graphviz" => Some(Self::Dot),
            _ => None,
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::Mermaid => "mermaid",
            Self::Dot => "dot",
        }
    }
}

#[async_trait]
impl Tool for GetDiagramTool {
    fn name(&self) -> &'static str {
        "get_diagram"
    }

    fn description(&self) -> &'static str {
        "Render an ER diagram of a connection's schema as Mermaid \
         (`erDiagram`) or Graphviz (`dot`). When `table` is given the \
         output is restricted to that table and its 1-hop foreign-key \
         neighbours; otherwise it covers the whole schema (or a single \
         schema if `schema` is set). Use after `describe_schema` to get \
         a human-readable view of FK relationships."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "connection": {
                    "type": "string",
                    "description": "Connection name as returned by `list_connections`."
                },
                "format": {
                    "type": "string",
                    "enum": ["mermaid", "dot"],
                    "description": "Output format. Default `mermaid`. \
                                    Mermaid pastes directly into mermaid.live; \
                                    `dot` runs through Graphviz (`dot -Tsvg`)."
                },
                "table": {
                    "type": "string",
                    "description": "Optional. When set, the diagram is restricted \
                                    to this table and its 1-hop FK neighbours. \
                                    May be `name` or `schema.name`."
                },
                "schema": {
                    "type": "string",
                    "description": "Optional. Restrict the diagram to a single schema. \
                                    Ignored when `table` is `schema.name`-qualified."
                }
            },
            "required": ["connection"],
            "additionalProperties": false,
        })
    }

    async fn call(&self, ctx: &ServerContext, arguments: Value) -> Result<ToolOutput, McpError> {
        let conn_name = arguments
            .get("connection")
            .and_then(Value::as_str)
            .ok_or_else(|| McpError::InvalidParams("missing `connection` argument".into()))?;
        let format_token = arguments
            .get("format")
            .and_then(Value::as_str)
            .unwrap_or("mermaid");
        let Some(format) = DiagramFormat::parse(format_token) else {
            return Ok(ToolOutput::err(format!(
                "unknown format: {format_token} (expected `mermaid` or `dot`)"
            )));
        };
        let table_arg = arguments.get("table").and_then(Value::as_str);
        let schema_arg = arguments.get("schema").and_then(Value::as_str);

        let mut conn = match ctx.open_connection(conn_name).await {
            Ok(c) => c,
            Err(McpError::UnknownConnection(_)) => {
                return Ok(ToolOutput::err(format!(
                    "unknown connection: {conn_name}. Call `list_connections` to see valid names."
                )));
            }
            Err(other) => return Err(other),
        };

        // Walk the schema tree once to know what to describe. Apply the
        // `schema` filter early so we don't waste round-trips on tables
        // outside the agent's interest.
        let tree = match conn.list_all_tables().await {
            Ok(tree) => tree,
            Err(error) => {
                let _ = conn.close().await;
                return Ok(ToolOutput::err(format!(
                    "schema introspection failed on `{conn_name}`: {error}"
                )));
            }
        };

        // Resolve the focus target *before* filtering by schema so a
        // qualified `schema.name` target can override an unrelated
        // `schema` argument.
        let resolved_target: Option<(String, String)> = if let Some(token) = table_arg {
            if let Some(pair) = resolve_table_in_tree(&tree, token, schema_arg) {
                Some(pair)
            } else {
                let _ = conn.close().await;
                return Ok(ToolOutput::err(format!(
                    "table not found in `{conn_name}`: {token}"
                )));
            }
        } else {
            None
        };

        // Build the candidate (schema, table) list. When a focus target
        // exists, restrict to its schema so cross-schema FKs (dropped by
        // `build()` in V1) do not leak in as dangling edges.
        let restrict_schema: Option<&str> = match (&resolved_target, schema_arg) {
            (Some((s, _)), _) => Some(s.as_str()),
            (None, Some(s)) => Some(s),
            (None, None) => None,
        };
        let pairs: Vec<(String, String)> = tree
            .iter()
            .filter(|(schema, _)| match restrict_schema {
                Some(want) => schema.name == want,
                None => true,
            })
            .flat_map(|(schema, tables)| {
                tables
                    .iter()
                    .map(move |t| (schema.name.clone(), t.name.clone()))
            })
            .collect();

        if pairs.is_empty() {
            let _ = conn.close().await;
            return Ok(ToolOutput::err(match restrict_schema {
                Some(s) => format!("connection `{conn_name}` has no tables in schema `{s}`"),
                None => format!("connection `{conn_name}` has no tables"),
            }));
        }

        // Describe each table serially through the borrowed connection.
        // MCP calls are short-lived (open / work / close) so we don't
        // open a pool here — the latency is dominated by the round-trips
        // themselves, not by sequencing them.
        let mut described = Vec::with_capacity(pairs.len());
        for (schema, table) in &pairs {
            match conn.describe_table(schema, table).await {
                Ok(t) => described.push(t),
                Err(error) => {
                    let _ = conn.close().await;
                    return Ok(ToolOutput::err(format!(
                        "describe_table failed on `{conn_name}.{schema}.{table}`: {error}"
                    )));
                }
            }
        }
        let _ = conn.close().await;

        // Logical relations come from `.narwhal/workspace.toml` (when
        // the MCP server was launched inside a workspace) and from
        // user-level `connections.toml`. Bad entries log to stderr but
        // do not fail the call — the diagram is still useful with the
        // valid subset.
        let workspace_root = ctx.workspace().map(|w| w.root.clone());
        let (logical, warnings) = collect_logical_relations_for(
            conn_name,
            ctx.connections(),
            workspace_root.as_deref(),
        );
        for w in &warnings {
            tracing::warn!(target: "narwhal::mcp::get_diagram", "{w}");
        }

        let (model, build_diags) = build_with_logical(&described, &logical);
        for d in &build_diags {
            tracing::warn!(target: "narwhal::mcp::get_diagram", "{d}");
        }
        let model = match resolved_target.as_ref() {
            Some((s, t)) => {
                let target = QualifiedName::new(s.clone(), t.clone());
                diagram_focused(&model, &target, 1)
            }
            None => model,
        };

        if model.is_empty() {
            return Ok(ToolOutput::err(
                "diagram is empty after filtering (check `schema` / `table` arguments)",
            ));
        }

        let title = match (&resolved_target, restrict_schema) {
            (Some((s, t)), _) => format!("narwhal: {s}.{t}"),
            (None, Some(s)) => format!("narwhal: schema {s}"),
            (None, None) => format!("narwhal: {conn_name}"),
        };

        let rendered = match format {
            DiagramFormat::Mermaid => MermaidRenderer::new().with_title(title).render(&model),
            DiagramFormat::Dot => DotRenderer::new().render(&model),
        };

        // Wrap the rendered diagram in a JSON envelope so the agent can
        // pick up metadata (node / edge counts) without re-parsing the
        // diagram source. Mermaid / DOT are returned in the `source`
        // string verbatim.
        let payload = json!({
            "connection": conn_name,
            "format": format.label(),
            "tables": model.nodes.len(),
            "edges": model.edges.len(),
            "focused_on": resolved_target
                .as_ref()
                .map(|(s, t)| format!("{s}.{t}")),
            "schema_filter": restrict_schema,
            "source": rendered,
        });
        let body = serde_json::to_string_pretty(&payload).unwrap_or_else(|_| "{}".to_string());
        let (body, _truncated) = cap_response(body, "get_diagram");
        Ok(ToolOutput::ok(body))
    }
}

/// Find the `(schema, table)` pair matching `token` in the schema tree.
///
/// Accepts `name` or `schema.name`. When `schema_hint` is set and the
/// token is unqualified we prefer the hinted schema; otherwise the
/// first match across all schemas wins.
fn resolve_table_in_tree(
    tree: &[(narwhal_core::schema::Schema, Vec<narwhal_core::schema::Table>)],
    token: &str,
    schema_hint: Option<&str>,
) -> Option<(String, String)> {
    if let Some((s, t)) = token.split_once('.') {
        return tree
            .iter()
            .find(|(schema, _)| schema.name == s)
            .and_then(|(_, tables)| tables.iter().find(|tbl| tbl.name == t))
            .map(|tbl| (s.to_owned(), tbl.name.clone()));
    }
    if let Some(hint) = schema_hint {
        if let Some(pair) = tree
            .iter()
            .find(|(schema, _)| schema.name == hint)
            .and_then(|(_, tables)| tables.iter().find(|tbl| tbl.name == token))
            .map(|tbl| (hint.to_owned(), tbl.name.clone()))
        {
            return Some(pair);
        }
    }
    for (schema, tables) in tree {
        if let Some(tbl) = tables.iter().find(|tbl| tbl.name == token) {
            return Some((schema.name.clone(), tbl.name.clone()));
        }
    }
    None
}
