//! Foreign-key navigation (v1.2 #6).
//!
//! `gd` (go-to-definition) on a cell whose column is part of a foreign
//! key opens the referenced row in a new run. Implementation:
//!
//! 1. Identify the focused (column, value) in the active result.
//! 2. Look up the source table's `TableSchema` (via `describe_table`
//!    on the active session). Skipped if not in a `Rows` result with
//!    a `RowSource`.
//! 3. Find any `ForeignKey` whose `columns` includes the focused
//!    column. If multiple FKs reference the same column the first
//!    one wins (rare in practice; a composite FK still picks the
//!    first column-to-column mapping).
//! 4. Build `SELECT * FROM <ref_schema>.<ref_table> WHERE <ref_col> = <value>`
//!    and dispatch it as a regular execute batch.
//!
//! `gr` (go-to-references) \u2014 finding tables that reference *this*
//! row \u2014 is a v1.2 follow-up; it requires walking every table's
//! FK list looking for back-references, which is fine on PG but
//! prohibitively expensive on schemas with hundreds of tables.

use narwhal_core::Value;

use super::{AppCore, ResultState};
use crate::run::RunMode;

impl AppCore {
    /// Handler for `gd` (go-to-definition) on the focused cell.
    pub(super) async fn fk_goto_definition(&mut self) {
        // Snapshot what we need before any await: column index, row
        // index, source (schema/table), full value.
        let snapshot = {
            let tab = &self.ui.tabs[self.ui.active_tab];
            let ResultState::Rows {
                columns,
                rows,
                source: Some(source),
                ..
            } = tab.results.active_state()
            else {
                self.ui.status.message = "fk: no editable rows here".into();
                return;
            };
            let col_idx = tab.results.active().column_index;
            let Some(column) = columns.get(col_idx) else {
                self.ui.status.message = "fk: no column under cursor".into();
                return;
            };
            let Some(row_idx) = self.selected_original_row_public().await else {
                self.ui.status.message = "fk: no row under cursor".into();
                return;
            };
            let Some(row) = rows.get(row_idx) else {
                self.ui.status.message = "fk: row index out of range".into();
                return;
            };
            let Some(value) = row.0.get(col_idx).cloned() else {
                self.ui.status.message = "fk: cell is empty".into();
                return;
            };
            if matches!(value, Value::Null) {
                self.ui.status.message = "fk: cell is NULL".into();
                return;
            }
            (
                source.schema.clone(),
                source.table.clone(),
                column.name.clone(),
                value,
            )
        };
        let (schema, table, column_name, value) = snapshot;

        // describe_table to fetch the FK list. We can't go through
        // session.column_cache (it only caches `Vec<ColumnHeader>`).
        let Some(session) = self.session.active.as_mut() else {
            self.ui.status.message = "fk: no active session".into();
            return;
        };
        let mut conn = match session.pool.acquire().await {
            Ok(c) => c,
            Err(e) => {
                self.ui.status.message = format!("fk: pool acquire failed: {e}");
                return;
            }
        };
        let schema_info = match conn.describe_table(&schema, &table).await {
            Ok(s) => s,
            Err(e) => {
                self.ui.status.message = format!("fk: describe_table failed: {e}");
                return;
            }
        };
        drop(conn);

        let Some((fk, col_pos)) = schema_info
            .foreign_keys
            .iter()
            .find_map(|fk| fk.columns.iter().position(|c| c == &column_name).map(|p| (fk, p)))
        else {
            self.ui.status.message =
                format!("fk: column '{column_name}' is not part of any foreign key");
            return;
        };

        let ref_schema = fk
            .referenced_schema
            .clone()
            .unwrap_or_else(|| schema.clone());
        let ref_table = fk.referenced_table.clone();
        let Some(ref_col) = fk.referenced_columns.get(col_pos) else {
            self.ui.status.message =
                "fk: composite FK with mismatched arity (parent column missing)".into();
            return;
        };
        let sql = format!(
            "SELECT * FROM {ref_schema}.{ref_table} WHERE {ref_col} = {}",
            render_literal(&value)
        );
        self.ui.status.message = format!("fk: -> {ref_schema}.{ref_table}");
        self.dispatch_batch(vec![sql], RunMode::Execute).await;
    }

    /// Wrapper that lets the `fk_nav` module call into the private
    /// helper on `results_actions.rs` without making that helper
    /// `pub` across the whole crate.
    async fn selected_original_row_public(&self) -> Option<usize> {
        let tab = &self.ui.tabs[self.ui.active_tab];
        let vis_selected = tab.results.active().selected()?;
        tab.results
            .active()
            .visible_indices
            .get(vis_selected)
            .copied()
    }
}

/// Render `value` as a SQL literal for the constructed WHERE clause.
///
/// Intentionally narrow: handles the integer / string / null path
/// FKs almost always take. Other types render via [`Value::render`]
/// which approximates the right form but isn't guaranteed safe for
/// every driver. The caller should treat the resulting SQL as
/// driver-best-effort, not as a parameterised query \u2014 a future
/// pass should route FK navigation through bound parameters.
fn render_literal(v: &Value) -> String {
    match v {
        Value::Null => "NULL".to_owned(),
        Value::Int(i) => i.to_string(),
        Value::Float(f) => f.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::String(s) => format!("'{}'", s.replace('\'', "''")),
        other => format!("'{}'", other.render().replace('\'', "''")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_literal_quotes_text_and_escapes() {
        assert_eq!(render_literal(&Value::Int(7)), "7");
        assert_eq!(render_literal(&Value::String("foo".into())), "'foo'");
        assert_eq!(
            render_literal(&Value::String("O'Brien".into())),
            "'O''Brien'"
        );
        assert_eq!(render_literal(&Value::Null), "NULL");
        assert_eq!(render_literal(&Value::Bool(true)), "true");
    }
}
