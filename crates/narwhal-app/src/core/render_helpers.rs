//! View-projection helpers extracted from `core.rs` (L21).
//!
//! These functions translate engine-side state ([`ResultState`],
//! [`SidebarItem`], explain results) into the pure-data views that
//! `narwhal-tui` understands. They never touch [`super::AppCore`] state.
//!
//! Keeping them in their own module makes the main file easier to scan
//! and lets the renderer evolve without dragging the entire event loop
//! along.
use narwhal_core::{ColumnHeader, Row, TableKind};
use narwhal_tui::{ResultDisplay, SearchHighlight, SidebarRowKind};

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
