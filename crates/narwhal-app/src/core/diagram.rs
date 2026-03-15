//! `:diagram export` command handler.
//!
//! Pulls full schemas from the active session, runs them through
//! [`narwhal_diagram`] and either writes the rendered Mermaid / DOT to
//! disk or copies it to the system clipboard.
//!
//! V1 is intentionally synchronous on the dispatcher's task — describing
//! a table is one round-trip per table and even prod-sized schemas
//! (~100 tables) finish in well under a second. The pattern matches
//! `:dump-schema` for the single-table case; the "all" target there
//! offloads to the meta channel and we will do the same in V2 if
//! benchmarks show it is needed.

use std::path::PathBuf;

use narwhal_diagram::{
    build, focused as diagram_focused, DiagramModel, DotRenderer, MermaidRenderer, QualifiedName,
    Renderer,
};
use narwhal_pool::Pool;

use super::AppCore;
use crate::commands::DiagramFormat;

impl AppCore {
    pub(super) async fn export_diagram(
        &mut self,
        format: DiagramFormat,
        path: Option<String>,
        table: Option<String>,
        schema: Option<String>,
    ) {
        let Some(session) = self.session.active.as_ref() else {
            self.ui.status.message = "diagram: no active connection".into();
            return;
        };

        // Collect candidate (schema, table) pairs from the sidebar
        // cache. The diagram filters happen here so we avoid describing
        // tables the user does not want.
        let mut pairs: Vec<(String, String)> = session
            .schemas
            .iter()
            .filter(|(s, _)| match schema.as_deref() {
                Some(want) => s.name == want,
                None => true,
            })
            .flat_map(|(s, tables)| {
                tables
                    .iter()
                    .map(move |t| (s.name.clone(), t.name.clone()))
            })
            .collect();

        // If the user asked for a focused diagram, keep the target table
        // + its 1-hop FK neighbours. We don't know FKs yet (those come
        // from describe_table), so this first pass keeps everything in
        // the same schema as the target and prunes later.
        if let Some(t) = table.as_deref() {
            let target = resolve_table(&pairs, t, schema.as_deref());
            let Some((target_schema, target_name)) = target else {
                self.ui.status.message = format!("diagram: table not found: {t}");
                return;
            };
            // Restrict to the target's schema; cross-schema FKs are
            // dropped by `build()` anyway in V1.
            pairs.retain(|(s, _)| s == &target_schema);
            // We still describe every table in that schema so 1-hop
            // neighbours can be detected, then `focused()` filters
            // the rendered model.
            // Carry the target through for the post-build filter.
            self.run_diagram(
                session.pool.clone(),
                pairs,
                format,
                path,
                Some(QualifiedName::new(target_schema, target_name)),
            )
            .await;
            return;
        }

        if pairs.is_empty() {
            self.ui.status.message = match schema.as_deref() {
                Some(s) => format!("diagram: schema '{s}' has no tables (or does not exist)"),
                None => "diagram: no tables in the active connection".into(),
            };
            return;
        }

        self.run_diagram(session.pool.clone(), pairs, format, path, None)
            .await;
    }

    async fn run_diagram(
        &mut self,
        pool: Pool,
        pairs: Vec<(String, String)>,
        format: DiagramFormat,
        path: Option<String>,
        focus: Option<QualifiedName>,
    ) {
        let count = pairs.len();
        self.ui.status.message = format!("diagram: describing {count} table(s)…");

        // Acquire one connection and walk the list serially. This keeps
        // load on the engine predictable; the alternative (pooled
        // describe_table fan-out) would race the pool's max_size limit
        // against the typical 2–5 connection ceiling.
        let described = describe_all(pool, pairs).await;
        let tables = match described {
            Ok(ts) => ts,
            Err(error) => {
                self.ui.status.message = format!("diagram: describe failed: {error}");
                return;
            }
        };

        let model = build(&tables);
        let model = match focus {
            Some(target) => diagram_focused(&model, &target, 1),
            None => model,
        };

        if model.is_empty() {
            self.ui.status.message = "diagram: nothing to render after filtering".into();
            return;
        }

        let rendered = render(&model, format);

        match path {
            Some(p) => self.write_diagram_file(&p, &rendered, format, &model),
            None => self.copy_diagram_to_clipboard(&rendered, format, &model).await,
        }
    }

    fn write_diagram_file(
        &mut self,
        path: &str,
        rendered: &str,
        format: DiagramFormat,
        model: &DiagramModel,
    ) {
        let mut path_buf = PathBuf::from(path);
        if path_buf.extension().is_none() {
            path_buf.set_extension(format.default_extension());
        }
        match std::fs::write(&path_buf, rendered) {
            Ok(()) => {
                self.ui.status.message = format!(
                    "diagram: wrote {} ({} tables, {} edges) to {}",
                    format.label(),
                    model.nodes.len(),
                    model.edges.len(),
                    path_buf.display(),
                );
            }
            Err(error) => {
                self.ui.status.message =
                    format!("diagram: write to {} failed: {error}", path_buf.display());
            }
        }
    }

    async fn copy_diagram_to_clipboard(
        &mut self,
        rendered: &str,
        format: DiagramFormat,
        model: &DiagramModel,
    ) {
        let clipboard = std::sync::Arc::clone(&self.deps.clipboard);
        match clipboard.set_text(rendered) {
            Ok(()) => {
                self.ui.status.message = format!(
                    "diagram: copied {} ({} tables, {} edges) to clipboard",
                    format.label(),
                    model.nodes.len(),
                    model.edges.len(),
                );
            }
            Err(error) => {
                self.ui.status.message = format!("diagram: clipboard write failed: {error}");
            }
        }
    }
}

fn render(model: &DiagramModel, format: DiagramFormat) -> String {
    match format {
        DiagramFormat::Mermaid => MermaidRenderer::new()
            .with_title("narwhal schema")
            .render(model),
        DiagramFormat::Dot => DotRenderer::new().render(model),
    }
}

/// Resolve a user-supplied table token (`users` or `public.users`) to a
/// concrete `(schema, table)` pair from the candidate list.
///
/// If the token contains a dot it is treated as fully qualified.
/// Otherwise we prefer `schema_hint` when set, then fall back to the
/// first match in any schema.
fn resolve_table(
    pairs: &[(String, String)],
    token: &str,
    schema_hint: Option<&str>,
) -> Option<(String, String)> {
    if let Some((s, t)) = token.split_once('.') {
        if pairs.iter().any(|(ps, pt)| ps == s && pt == t) {
            return Some((s.to_owned(), t.to_owned()));
        }
        return None;
    }
    if let Some(s) = schema_hint {
        if let Some(p) = pairs.iter().find(|(ps, pt)| ps == s && pt == token) {
            return Some(p.clone());
        }
    }
    pairs.iter().find(|(_, pt)| pt == token).cloned()
}

async fn describe_all(
    pool: Pool,
    pairs: Vec<(String, String)>,
) -> Result<Vec<narwhal_core::schema::TableSchema>, narwhal_core::Error> {
    let mut conn = pool
        .acquire()
        .await
        .map_err(|e| narwhal_core::Error::Connection(e.to_string()))?;
    let mut out = Vec::with_capacity(pairs.len());
    for (schema, table) in pairs {
        out.push(conn.describe_table(&schema, &table).await?);
    }
    Ok(out)
}
