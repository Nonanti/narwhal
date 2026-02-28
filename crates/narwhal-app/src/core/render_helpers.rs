//! View-projection helpers extracted from `core.rs` (L21).
//!
//! These functions translate engine-side state ([`ResultState`],
//! [`SidebarItem`], explain results) into the pure-data views that
//! `narwhal-tui` understands. They never touch [`super::AppCore`] state.
//!
//! Keeping them in their own module makes the main file easier to scan
//! and lets the renderer evolve without dragging the entire event loop
//! along.
use narwhal_core::{ColumnHeader, ConnectionColor, Row, TableKind};
use narwhal_tui::{ExplainPlanLine, ResultDisplay, SearchHighlight, SidebarRowKind};

use crate::explain::ExplainNode;

/// v1.1 #3: flatten an [`ExplainNode`] tree into the line list the
/// renderer consumes. Computes per-line metadata:
///
/// - `connector` — unicode box-drawing prefix that produces a real
///   tree visual (`├─`, `└─`, `│ `) instead of indented bullets.
/// - `cost_ratio` — normalised against the plan's max cost so each
///   node's bar reflects its relative weight.
/// - `hot` — `true` for the chain root → highest-cost child → …
/// - `divergent` — forwarded from
///   [`ExplainNode::rows_divergent`].
#[must_use]
pub(super) fn explain_tree_lines(root: &ExplainNode) -> Vec<ExplainPlanLine> {
    let max_cost = root.max_cost().max(1.0);
    let mut out = Vec::new();
    let mut stack: Vec<bool> = Vec::new();
    let mut hot = std::collections::HashSet::new();
    mark_hot_path(root, &mut hot);
    walk_tree(root, &mut stack, &mut out, max_cost, &hot, true, true);
    out
}

/// Recursive worker: emits one line per node with a box-drawing
/// connector built from the ancestor `is_last` flags.
#[allow(clippy::too_many_arguments)] // recursion depth + state; clear is more important here
fn walk_tree(
    node: &ExplainNode,
    stack: &mut Vec<bool>,
    out: &mut Vec<ExplainPlanLine>,
    max_cost: f64,
    hot: &std::collections::HashSet<*const ExplainNode>,
    is_root: bool,
    is_last: bool,
) {
    // Build the connector for this row from the ancestor stack.
    let connector = build_connector(stack, is_root, is_last);
    let cost_ratio = if max_cost > 0.0 {
        Some((node.total_cost / max_cost).clamp(0.0, 1.0))
    } else {
        None
    };
    out.push(ExplainPlanLine {
        depth: stack.len(),
        text: node.label(),
        cost_ratio,
        hot: hot.contains(&(node as *const _)),
        divergent: node.rows_divergent(),
        connector,
    });
    let last_idx = node.children.len().saturating_sub(1);
    for (i, child) in node.children.iter().enumerate() {
        stack.push(is_last);
        walk_tree(child, stack, out, max_cost, hot, false, i == last_idx);
        stack.pop();
    }
}

/// Build the box-drawing prefix from the ancestor `is_last` stack.
///
/// Each level contributes `"│ "` (more siblings come) or `"  "`
/// (last child at that level), and the current row appends
/// `"├─ "` / `"└─ "`. The root is rendered with a flat `"▸ "` so
/// it stands out.
fn build_connector(stack: &[bool], is_root: bool, is_last: bool) -> String {
    if is_root {
        return "  ▸ ".to_owned();
    }
    let mut s = String::from("  ");
    for &ancestor_last in stack.iter().skip(1) {
        s.push_str(if ancestor_last { "  " } else { "│ " });
    }
    s.push_str(if is_last { "└─ " } else { "├─ " });
    s
}

/// Walk the highest-cost branch from the root to a leaf and mark each
/// node on it as hot. Identifies the path the planner spent the most
/// budget on.
fn mark_hot_path(node: &ExplainNode, hot: &mut std::collections::HashSet<*const ExplainNode>) {
    hot.insert(node as *const _);
    if let Some(child) = node
        .children
        .iter()
        .max_by(|a, b| a.total_cost.partial_cmp(&b.total_cost).unwrap_or(std::cmp::Ordering::Equal))
    {
        mark_hot_path(child, hot);
    }
}

/// v1.1 #2: project [`ConnectionColor`] (config domain) onto the
/// `ratatui::Color` palette so the renderer doesn't depend on
/// `narwhal-core`. The fixed six-colour mapping is intentional —
/// hex/RGB introduces terminal-compat surprises we'd rather not
/// carry in v1.
#[must_use]
pub(super) const fn connection_color_to_ratatui(c: ConnectionColor) -> ratatui::style::Color {
    use ratatui::style::Color;
    match c {
        ConnectionColor::Red => Color::Red,
        ConnectionColor::Yellow => Color::Yellow,
        ConnectionColor::Green => Color::Green,
        ConnectionColor::Blue => Color::Blue,
        ConnectionColor::Magenta => Color::Magenta,
        ConnectionColor::Cyan => Color::Cyan,
        // `ConnectionColor` is #[non_exhaustive] so future variants
        // (added by a downstream crate or a v2 feature) fall back to
        // a neutral grey rather than panicking.
        _ => Color::Gray,
    }
}

use super::{ResultState, SidebarItem};
use crate::explain::{parse as parse_plan, ExplainPlan};

/// Postgres `EXPLAIN (FORMAT JSON)` returns a single `"QUERY PLAN"`
/// column of `Value::Json`. Detect that shape so the run loop can
/// re-parse it as a plan tree.
pub(super) fn is_explain_result(columns: &[ColumnHeader]) -> bool {
    columns.len() == 1 && columns[0].name.eq_ignore_ascii_case("QUERY PLAN")
}

/// Extract the plan JSON from the first row of an explain-shaped result
/// and parse it.
pub(super) fn extract_explain_plan(rows: &[Row]) -> Result<ExplainPlan, String> {
    let row = rows
        .first()
        .ok_or_else(|| "empty explain result".to_owned())?;
    let value = row
        .0
        .first()
        .ok_or_else(|| "explain row missing column".to_owned())?;
    let json_text = match value {
        narwhal_core::Value::Json(v) => v.to_string(),
        narwhal_core::Value::String(s) | narwhal_core::Value::Unknown(s) => s.clone(),
        other => other.render(),
    };
    parse_plan(&json_text)
}

/// Project [`ResultState`] into the renderer's borrowed-view type.
pub(super) fn display_from_state<'a>(
    state: &'a ResultState,
    search: Option<&'a SearchHighlight<'a>>,
) -> ResultDisplay<'a> {
    match state {
        ResultState::Empty => ResultDisplay::Empty,
        ResultState::Running {
            sql,
            index,
            total,
            columns,
            rows,
            streaming,
            started_at,
            ..
        } => ResultDisplay::Running {
            sql,
            index: *index,
            total: *total,
            columns,
            rows,
            streaming: *streaming,
            started_at: *started_at,
        },
        ResultState::Affected {
            rows,
            elapsed_ms,
            index,
            total,
        } => ResultDisplay::Affected {
            rows: *rows,
            elapsed_ms: *elapsed_ms,
            index: *index,
            total: *total,
        },
        ResultState::Rows {
            columns,
            rows,
            elapsed_ms,
            streamed,
            index,
            total,
            source: _,
            source_table: _,
        } => ResultDisplay::Rows {
            columns,
            rows,
            elapsed_ms: *elapsed_ms,
            streamed: *streamed,
            index: *index,
            total: *total,
            search,
        },
        ResultState::Explain {
            lines,
            planning_time_ms,
            execution_time_ms,
        } => ResultDisplay::Explain {
            lines,
            planning_time_ms: *planning_time_ms,
            execution_time_ms: *execution_time_ms,
        },
        ResultState::TableDetail {
            schema,
            active_meta_tab,
        } => ResultDisplay::TableDetail {
            schema,
            active_tab: *active_meta_tab,
        },
        ResultState::Cancelled {
            rows_so_far,
            elapsed_ms,
        } => ResultDisplay::Cancelled {
            rows_so_far: *rows_so_far,
            elapsed_ms: *elapsed_ms,
        },
        ResultState::Error {
            message,
            elapsed_ms,
        } => ResultDisplay::Error {
            message,
            elapsed_ms: *elapsed_ms,
        },
    }
}

/// Human-readable label for a sidebar row.
pub(super) fn sidebar_label(item: &SidebarItem) -> String {
    match item {
        SidebarItem::Connection { name, driver, .. } => format!("{name} ({driver})"),
        SidebarItem::Schema { name } => name.clone(),
        SidebarItem::Table { name, .. } => name.clone(),
    }
}

/// Indentation depth (in tree levels) of a sidebar row.
pub(super) const fn sidebar_depth(item: &SidebarItem) -> u8 {
    match item {
        SidebarItem::Connection { .. } => 0,
        SidebarItem::Schema { .. } => 1,
        SidebarItem::Table { .. } => 2,
    }
}

/// Visual classification (icon/colour) for a sidebar row.
pub(super) const fn sidebar_kind(item: &SidebarItem) -> SidebarRowKind {
    match item {
        SidebarItem::Connection { active: true, .. } => SidebarRowKind::ActiveConnection,
        SidebarItem::Connection { .. } => SidebarRowKind::Connection,
        SidebarItem::Schema { .. } => SidebarRowKind::Schema,
        SidebarItem::Table { kind, .. } => match kind {
            TableKind::Table => SidebarRowKind::Table,
            TableKind::View => SidebarRowKind::View,
            TableKind::MaterializedView => SidebarRowKind::MaterializedView,
            TableKind::SystemTable => SidebarRowKind::SystemTable,
            // Future TableKind variants: classify as a regular table.
            _ => SidebarRowKind::Table,
        },
    }
}
