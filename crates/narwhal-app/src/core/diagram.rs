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

use narwhal_config::{collect_logical_relations_for, DiagramIcons};
use narwhal_diagram::{
    build_with_logical, focused as diagram_focused, impact as diagram_impact, DiagramModel,
    DotRenderer, IconSet, LogicalRelation, MermaidRenderer, QualifiedName, Renderer,
};
use narwhal_pool::Pool;

use super::state::result::{DiagramModalState, DiagramMode};
use super::AppCore;
use crate::commands::DiagramFormat;

impl AppCore {
    /// Open the diagram modal in Focused mode, centred on `table`.
    ///
    /// `table` may be `name` or `schema.name`. The full schema is
    /// described once and cached on the modal state so re-centering
    /// (Enter on a neighbour) is instant.
    pub(super) async fn open_diagram_focus(&mut self, table: String) {
        self.open_diagram_modal(&table, DiagramMode::Focused).await;
    }

    /// Open the diagram modal in Impact mode (reverse-FK closure).
    pub(super) async fn open_diagram_impact(&mut self, table: String) {
        self.open_diagram_modal(&table, DiagramMode::Impact).await;
    }

    async fn open_diagram_modal(&mut self, table: &str, mode: DiagramMode) {
        let Some(session) = self.session.active.as_ref() else {
            self.ui.status.message = "diagram: no active connection".into();
            return;
        };

        // Restrict to the target's schema so the cached model only
        // describes tables that can actually appear in the modal
        // (cross-schema FKs are dropped by `build()` in V1).
        //
        // # Why describe every table in the schema, not just the 1-hop
        // # neighbours?
        //
        // The modal caches the *full* schema model so the user can:
        //   - Press `Enter` to re-center on a neighbour (instant),
        //   - Press `i` to toggle into Impact mode (needs reverse-FK
        //     closure, which needs every table to be present),
        //   - Press `y` to yank a Mermaid rendering of the full
        //     impact tree.
        // A lazy two-pass strategy (describe target → describe its FK
        // targets) would shave the first-paint latency in big schemas
        // but force a re-describe on every re-center / mode toggle.
        // Benchmark of a 200-table Postgres prod schema: ~0.6s for the
        // full describe, ~0s for every subsequent navigation. The
        // trade-off is worth it; revisit if a real schema crosses
        // 1000+ tables.
        let pairs_all: Vec<(String, String)> = session
            .schemas
            .iter()
            .flat_map(|(s, tables)| tables.iter().map(move |t| (s.name.clone(), t.name.clone())))
            .collect();

        let Some((target_schema, target_name)) = resolve_table(&pairs_all, table, None) else {
            self.ui.status.message = format!("diagram: table not found: {table}");
            return;
        };
        let pairs: Vec<(String, String)> = pairs_all
            .into_iter()
            .filter(|(s, _)| s == &target_schema)
            .collect();

        self.ui.status.message = format!("diagram: describing {} table(s)…", pairs.len());

        let pool = session.pool.clone();
        let described = describe_all(pool, pairs).await;
        let tables = match described {
            Ok(ts) => ts,
            Err(error) => {
                self.ui.status.message = format!("diagram: describe failed: {error}");
                return;
            }
        };

        let (logical, relation_warnings) = self.collect_logical_for_active();
        let (model, build_diags) = build_with_logical(&tables, &logical);
        // Surface logical-relation diagnostics first so the user sees
        // typos in their config before the success message overwrites
        // the status bar.
        for w in relation_warnings.iter().chain(
            build_diags
                .iter()
                .map(std::string::ToString::to_string)
                .collect::<Vec<_>>()
                .iter(),
        ) {
            tracing::warn!(target: "narwhal::app::diagram", "{w}");
        }
        if !build_diags.is_empty() {
            self.ui.status.message = format!(
                "diagram: {} logical-relation issue(s); see log",
                build_diags.len()
            );
        }
        let center = QualifiedName::new(target_schema, target_name);
        if model.node(&center).is_none() {
            self.ui.status.message = format!(
                "diagram: table not found in cached model: {}",
                center.display()
            );
            return;
        }
        let impact_tree = diagram_impact(&model, &center);
        let icons = icon_set_from(self.ui.diagram_icons);

        let label = match mode {
            DiagramMode::Focused => "focused",
            DiagramMode::Impact => "impact",
        };
        self.ui.status.message = format!(
            "diagram ({label}): {} — {} tables, {} edges",
            center.display(),
            model.nodes.len(),
            model.edges.len()
        );

        let active = self.ui.active_tab;
        self.ui.tabs[active].diagram = Some(DiagramModalState {
            mode,
            model,
            center,
            impact: impact_tree,
            selected: 0,
            scroll: 0,
            icons,
        });
    }

    /// Move the centre of the Focused modal to the currently-selected
    /// neighbour. No-op when the user is not in Focused mode or has no
    /// neighbours to centre on.
    pub(super) fn diagram_recenter_selected(&mut self) {
        let active = self.ui.active_tab;
        let Some(state) = self.ui.tabs[active].diagram.as_mut() else {
            return;
        };
        if state.mode != DiagramMode::Focused {
            return;
        }
        let outbound: Vec<&narwhal_diagram::Edge> = state
            .model
            .edges
            .iter()
            .filter(|e| e.from == state.center)
            .collect();
        let inbound: Vec<&narwhal_diagram::Edge> = state
            .model
            .edges
            .iter()
            .filter(|e| e.to == state.center)
            .collect();
        let total = outbound.len() + inbound.len();
        if total == 0 {
            return;
        }
        let selected = state.selected.min(total - 1);
        let new_center = if selected < outbound.len() {
            outbound[selected].to.clone()
        } else {
            inbound[selected - outbound.len()].from.clone()
        };
        if new_center == state.center {
            return;
        }
        state.center = new_center;
        state.impact = diagram_impact(&state.model, &state.center);
        state.selected = 0;
        state.scroll = 0;
        self.ui.status.message = format!("diagram: re-centred on {}", state.center.display());
    }

    /// Toggle between Focused and Impact mode for the open modal.
    pub(super) fn diagram_toggle_mode(&mut self) {
        let active = self.ui.active_tab;
        let Some(state) = self.ui.tabs[active].diagram.as_mut() else {
            return;
        };
        state.mode = match state.mode {
            DiagramMode::Focused => DiagramMode::Impact,
            DiagramMode::Impact => DiagramMode::Focused,
        };
        state.scroll = 0;
    }

    /// Yank a Mermaid rendering of the *current* modal view to the
    /// clipboard. In Focused mode this is the 1-hop subset; in Impact
    /// mode it is the full cached model with a comment header listing
    /// the impact tree (the tree itself is text not Mermaid — so we
    /// just dump the model and prepend a hint).
    pub(super) fn diagram_yank_mermaid(&mut self) {
        let active = self.ui.active_tab;
        let Some(state) = self.ui.tabs[active].diagram.as_ref() else {
            return;
        };
        let subset = match state.mode {
            DiagramMode::Focused => diagram_focused(&state.model, &state.center, 1),
            DiagramMode::Impact => state.model.clone(),
        };
        let mermaid = MermaidRenderer::new()
            .with_title(format!("narwhal: {}", state.center.display()))
            .render(&subset);
        match self.deps.clipboard.set_text(&mermaid) {
            Ok(()) => {
                self.ui.status.message = format!(
                    "diagram: yanked mermaid ({} tables, {} edges)",
                    subset.nodes.len(),
                    subset.edges.len()
                );
            }
            Err(error) => {
                self.ui.status.message = format!("diagram: clipboard write failed: {error}");
            }
        }
    }

    /// Cycle selection forward (or backward with `delta = -1`) inside
    /// the Focused-mode neighbour list. No-op in Impact mode.
    pub(super) fn diagram_cycle_selection(&mut self, forward: bool) {
        let active = self.ui.active_tab;
        let Some(state) = self.ui.tabs[active].diagram.as_mut() else {
            return;
        };
        if state.mode != DiagramMode::Focused {
            return;
        }
        let total = state
            .model
            .edges
            .iter()
            .filter(|e| e.from == state.center || e.to == state.center)
            .count();
        if total == 0 {
            return;
        }
        let new_sel = if forward {
            (state.selected + 1) % total
        } else {
            (state.selected + total - 1) % total
        };
        state.selected = new_sel;
    }

    /// Collect logical relations for the currently-active session from
    /// `connections.toml` and (when present) the workspace cached at
    /// startup. Returns an empty vec + empty warnings when no session
    /// is active so callers never have to special-case the
    /// disconnected state.
    ///
    /// The workspace root is *not* re-discovered on each call — every
    /// `:diagram` invocation sees the same boundary the binary was
    /// launched in, matching the MCP server's once-at-start behaviour
    /// (`ctx.workspace().root`).
    fn collect_logical_for_active(&self) -> (Vec<LogicalRelation>, Vec<String>) {
        let Some(session) = self.session.active.as_ref() else {
            return (Vec::new(), Vec::new());
        };
        collect_logical_relations_for(
            &session.config.name,
            &self.session.connections,
            self.session.workspace_root.as_deref(),
        )
    }

    /// Close the diagram modal (Esc / q).
    pub(super) fn diagram_close(&mut self) {
        let active = self.ui.active_tab;
        if self.ui.tabs[active].diagram.take().is_some() {
            self.ui.status.message = "diagram closed".into();
        }
    }

    /// Modal handler for the diagram overlay. Owns its own key
    /// vocabulary; the configured keymap is *not* consulted here —
    /// the modal's reflexes (`Esc`/`q` closes, etc.) are fixed for
    /// the same reason JSON viewer's are: removing them would
    /// trap the user inside the modal.
    pub(super) async fn handle_diagram_key(&mut self, key: crossterm::event::KeyEvent) {
        use crossterm::event::{KeyCode, KeyModifiers};

        let active = self.ui.active_tab;
        // Re-check on every key so the borrow stays scoped.
        if self.ui.tabs[active].diagram.is_none() {
            return;
        }

        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => self.diagram_close(),
            KeyCode::Tab => self.diagram_cycle_selection(true),
            KeyCode::BackTab => self.diagram_cycle_selection(false),
            KeyCode::Char('j') | KeyCode::Down => self.diagram_cycle_selection(true),
            KeyCode::Char('k') | KeyCode::Up => self.diagram_cycle_selection(false),
            KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if let Some(state) = self.ui.tabs[active].diagram.as_mut() {
                    state.scroll = state.scroll.saturating_add(10);
                }
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if let Some(state) = self.ui.tabs[active].diagram.as_mut() {
                    state.scroll = state.scroll.saturating_sub(10);
                }
            }
            KeyCode::Char('g') => {
                if let Some(state) = self.ui.tabs[active].diagram.as_mut() {
                    state.scroll = 0;
                }
            }
            KeyCode::Char('G') => {
                // Render-time clamp handles the upper bound; pick a
                // large sentinel here rather than recomputing the line
                // count off the model.
                if let Some(state) = self.ui.tabs[active].diagram.as_mut() {
                    state.scroll = u16::MAX;
                }
            }
            KeyCode::Enter => self.diagram_recenter_selected(),
            KeyCode::Char('i') => self.diagram_toggle_mode(),
            KeyCode::Char('y') => self.diagram_yank_mermaid(),
            KeyCode::Char('e') => {
                self.ui.status.message =
                    "diagram: use `:diagram export mermaid|dot [path]` for file output".into();
            }
            _ => {}
        }
    }

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
            .flat_map(|(s, tables)| tables.iter().map(move |t| (s.name.clone(), t.name.clone())))
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

        let (logical, relation_warnings) = self.collect_logical_for_active();
        let (model, build_diags) = build_with_logical(&tables, &logical);
        for w in &relation_warnings {
            tracing::warn!(target: "narwhal::app::diagram", "{w}");
        }
        for d in &build_diags {
            tracing::warn!(target: "narwhal::app::diagram", "{d}");
        }
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
            None => {
                self.copy_diagram_to_clipboard(&rendered, format, &model)
                    .await;
            }
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

const fn icon_set_from(icons: DiagramIcons) -> IconSet {
    // `DiagramIcons` is `#[non_exhaustive]`; fall back to Ascii for any
    // future variant we have not seen so terminals without a Nerd Font
    // never see broken glyphs.
    match icons {
        DiagramIcons::Nerdfont => IconSet::Nerdfont,
        DiagramIcons::Ascii | _ => IconSet::Ascii,
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
