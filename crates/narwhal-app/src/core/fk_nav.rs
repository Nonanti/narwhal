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

use narwhal_commands::cell_edit::{placeholder, quote_ident, quote_qualified};
use narwhal_core::Value;

use super::{AppCore, ResultState};
use crate::run::RunMode;

impl AppCore {
    /// Handler for `gd` (go-to-definition) on the focused cell.
    pub(super) async fn fk_goto_definition(&mut self) {
        // Snapshot what we need before any await: the focused
        // column's name, the source (schema/table), the row's
        // column names (so we can look up sibling values for a
        // composite FK), and the row's values themselves.
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
            // M-B: for composite FKs we'll need every column's name
            // and the row's full value vector — the focused cell
            // alone is not enough to safely land on the parent row
            // (a multi-tenant `(tenant_id, order_id)` FK would
            // otherwise leak rows across tenants when only
            // `order_id` was bound).
            let column_names: Vec<String> = columns.iter().map(|c| c.name.clone()).collect();
            let row_values = row.0.clone();
            (
                source.schema.clone(),
                source.table.clone(),
                column.name.clone(),
                column_names,
                row_values,
            )
        };
        let (schema, table, focused_col_name, column_names, row_values) = snapshot;

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

        // M-B: find the foreign key that includes the focused
        // column. The whole FK — not just the focused leg — is
        // what we'll filter the parent table on.
        let Some(fk) = schema_info
            .foreign_keys
            .iter()
            .find(|fk| fk.columns.iter().any(|c| c == &focused_col_name))
        else {
            self.ui.status.message =
                format!("fk: column '{focused_col_name}' is not part of any foreign key");
            return;
        };
        if fk.columns.len() != fk.referenced_columns.len() {
            self.ui.status.message =
                "fk: composite FK with mismatched arity (parent/child columns out of step)".into();
            return;
        }

        let ref_schema = fk
            .referenced_schema
            .clone()
            .unwrap_or_else(|| schema.clone());
        let ref_table = fk.referenced_table.clone();

        let dialect = self
            .session
            .active
            .as_ref()
            .map_or_else(narwhal_sql::splitter::Dialect::default, |s| s.dialect());

        // M-B: collect one (parent_col, child_value) binding per
        // FK leg. Bail out (and tell the user why) if any leg's
        // child column isn't in the result row — a partial
        // composite FK lookup is worse than no lookup at all
        // because the wrong-arity SELECT could return another
        // tenant's data.
        let mut bindings: Vec<FkLeg> = Vec::with_capacity(fk.columns.len());
        for (child_col, parent_col) in fk.columns.iter().zip(fk.referenced_columns.iter()) {
            let Some(idx) = column_names.iter().position(|c| c == child_col) else {
                self.ui.status.message = format!(
                    "fk: composite FK includes '{child_col}' which is not in the current result — SELECT it and retry"
                );
                return;
            };
            let Some(value) = row_values.get(idx).cloned() else {
                self.ui.status.message = format!("fk: cell for '{child_col}' missing from row");
                return;
            };
            if matches!(value, Value::Null) {
                self.ui.status.message = format!("fk: cell '{child_col}' is NULL");
                return;
            }
            bindings.push(FkLeg {
                parent_col_quoted: quote_ident(parent_col, dialect),
                value,
            });
        }

        // C2: identifier quoting + bound parameters. The previous
        // implementation interpolated schema/table/column straight
        // into the SQL and inlined the cell value via
        // `render_literal`, so a malicious value (or a quirky
        // identifier with a `"` in it) could break out of the
        // string literal and inject DDL. Every identifier is now
        // dialect-quoted and every cell value rides through the
        // driver's prepared-statement path.
        let qualified = quote_qualified(&ref_schema, &ref_table, dialect);
        let (sql, values) = build_fk_select_sql(&qualified, &bindings, dialect);
        self.ui.status.message = if bindings.len() > 1 {
            format!(
                "fk: -> {ref_schema}.{ref_table} ({} FK columns)",
                bindings.len()
            )
        } else {
            format!("fk: -> {ref_schema}.{ref_table}")
        };
        self.dispatch_batch_with_params(vec![(sql, values)], RunMode::Execute)
            .await;
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

/// One leg of a foreign key: the parent (referenced) column already
/// quoted for the active dialect, and the bound child value.
#[derive(Debug, Clone)]
struct FkLeg {
    parent_col_quoted: String,
    value: Value,
}

/// True when `value` binds to `PostgreSQL` as `text` and so needs a
/// matching `column::text` cast to compare against a non-text FK
/// column (uuid / numeric / inet / json / …). Variants the
/// driver binds at their native type (`Int` → `int8`, `Uuid` →
/// `uuid`, `Json` → `jsonb`, `Date`/`Time`/`DateTime`/`Timestamp`
/// → the matching chrono type, `Bytes` → `bytea`, `Bool` → `bool`)
/// keep the direct equality path so the FK index can still be
/// used. `Null` never reaches here — the snapshot bails before
/// composing the SQL.
///
/// CR-2 originally cast only `Value::String`; that missed
/// `Value::Unknown` which is the fallback the drivers fall back to
/// for unmapped types. An `Unknown("…")` cell against a `uuid` FK
/// column hit the same `operator does not exist: uuid = text`
/// planner error CR-2 was meant to fix.
const fn needs_pg_text_cast(value: &Value) -> bool {
    matches!(value, Value::String(_) | Value::Unknown(_))
}

/// Compose the FK navigation SELECT for one or more bound legs.
///
/// `qualified` is the dialect-quoted parent `schema.table`,
/// `bindings` is the list of `(parent_col_quoted, value)` pairs (one
/// per FK leg — a singleton for the common case, more for a
/// composite FK), and `dialect` selects the cast strategy.
///
/// Returns `(sql, values)`: the SQL with one placeholder per leg,
/// and the `Vec<Value>` ready to thread through
/// `dispatch_batch_with_params`.
///
/// **M-B (composite FKs):** every leg of the FK is bound. A
/// composite `(tenant_id, order_id) -> orders(tenant_id, id)` FK
/// produces `WHERE "tenant_id" = $1 AND "id" = $2`. Filtering on
/// just the focused leg would land on a sibling tenant's row that
/// happens to share the order id; binding every leg keeps the
/// lookup precise and tenant-safe.
///
/// **CR-2 (PG type strictness):** `PostgreSQL` does not implicit-cast
/// between text parameters and non-text columns. Sending
/// `WHERE uuid_col = $1` with a `Value::String("…")` cell value
/// fails at the planner with `operator does not exist: uuid = text`
/// even though the literal would compare equal. The conservative
/// fix is to cast the *column* to text when the bound value is a
/// string — it works for every column type, the typed driver bind
/// still happens (so prepared-statement safety stays), and the only
/// cost is losing the FK index on this leg. The cast is decided
/// per-leg so a `(numeric, text)` composite emits a direct equality
/// for the numeric leg and a `::text` cast for the text leg.
///
/// Numeric / boolean / null bindings keep the direct equality path.
/// Other dialects have looser typing (`MySQL`, `SQLite`, `DuckDB`,
/// `ClickHouse`) and do the implicit conversion themselves.
fn build_fk_select_sql(
    qualified: &str,
    bindings: &[FkLeg],
    dialect: narwhal_sql::splitter::Dialect,
) -> (String, Vec<Value>) {
    let mut sql = format!("SELECT * FROM {qualified} WHERE ");
    let mut values = Vec::with_capacity(bindings.len());
    for (i, leg) in bindings.iter().enumerate() {
        if i > 0 {
            sql.push_str(" AND ");
        }
        let ph = placeholder(i + 1, dialect);
        let cast_text = matches!(dialect, narwhal_sql::splitter::Dialect::Postgres)
            && needs_pg_text_cast(&leg.value);
        if cast_text {
            sql.push_str(&format!("{}::text = {ph}", leg.parent_col_quoted));
        } else {
            sql.push_str(&format!("{} = {ph}", leg.parent_col_quoted));
        }
        values.push(leg.value.clone());
    }
    (sql, values)
}

#[cfg(test)]
mod tests {
    use narwhal_commands::cell_edit::{placeholder, quote_ident, quote_qualified};
    use narwhal_core::Value;
    use narwhal_sql::splitter::Dialect;

    use super::{build_fk_select_sql, FkLeg};

    fn leg(col_quoted: &str, value: Value) -> FkLeg {
        FkLeg {
            parent_col_quoted: col_quoted.to_owned(),
            value,
        }
    }

    /// C2 regression: malicious identifiers and cell values used to
    /// reach the driver as raw SQL. Verify the building blocks now
    /// produce a parametric statement with quoted identifiers.
    #[test]
    fn fk_query_uses_quoted_identifiers_and_placeholder() {
        let qualified = quote_qualified("pub\"lic", "us\"ers", Dialect::Postgres);
        assert_eq!(qualified, r#""pub""lic"."us""ers""#);
        let col = quote_ident("id", Dialect::Postgres);
        let ph = placeholder(1, Dialect::Postgres);
        let sql = format!("SELECT * FROM {qualified} WHERE {col} = {ph}");
        assert_eq!(sql, r#"SELECT * FROM "pub""lic"."us""ers" WHERE "id" = $1"#);
    }

    #[test]
    fn fk_query_placeholder_dialect_aware() {
        assert_eq!(placeholder(1, Dialect::Postgres), "$1");
        assert_eq!(placeholder(1, Dialect::MySql), "?");
        assert_eq!(placeholder(1, Dialect::Sqlite), "?");
    }

    /// CR-2 regression: PG with a String value casts the COLUMN to
    /// text so a uuid / numeric column accepts the text-bound
    /// parameter without the planner refusing the comparison.
    #[test]
    fn pg_string_value_casts_column_to_text() {
        let (sql, values) = build_fk_select_sql(
            r#""public"."users""#,
            &[leg(r#""id""#, Value::String("abcd-1234-uuid-like".into()))],
            Dialect::Postgres,
        );
        assert_eq!(
            sql,
            r#"SELECT * FROM "public"."users" WHERE "id"::text = $1"#
        );
        assert_eq!(values.len(), 1);
    }

    /// PG numeric / boolean / null cell values stay on the direct
    /// equality path — the driver binds them at their native type
    /// and the planner is happy.
    #[test]
    fn pg_int_value_keeps_direct_equality() {
        let (sql, values) = build_fk_select_sql(
            r#""public"."users""#,
            &[leg(r#""id""#, Value::Int(7))],
            Dialect::Postgres,
        );
        assert_eq!(sql, r#"SELECT * FROM "public"."users" WHERE "id" = $1"#);
        assert_eq!(values.len(), 1);
    }

    /// Other dialects are loose-typed: no cast is emitted even when
    /// the cell value is a string.
    #[test]
    fn other_dialects_do_not_cast_string_value() {
        for dialect in [Dialect::Sqlite, Dialect::MySql, Dialect::Generic] {
            let (sql, _) = build_fk_select_sql(
                "`users`",
                &[leg("`id`", Value::String("x".into()))],
                dialect,
            );
            assert_eq!(
                sql, "SELECT * FROM `users` WHERE `id` = ?",
                "dialect {dialect:?} should not emit a ::text cast"
            );
        }
    }

    /// M-B regression: a composite FK binds every leg with `AND`
    /// joins so the parent lookup is tenant-safe. The previous
    /// implementation only bound the focused leg, which could land
    /// on the wrong row for a multi-column FK.
    #[test]
    fn composite_fk_binds_every_leg() {
        let (sql, values) = build_fk_select_sql(
            r#""public"."orders""#,
            &[
                leg(r#""tenant_id""#, Value::Int(7)),
                leg(r#""id""#, Value::Int(42)),
            ],
            Dialect::Postgres,
        );
        assert_eq!(
            sql,
            r#"SELECT * FROM "public"."orders" WHERE "tenant_id" = $1 AND "id" = $2"#
        );
        assert_eq!(values.len(), 2);
        assert!(matches!(values[0], Value::Int(7)));
        assert!(matches!(values[1], Value::Int(42)));
    }

    /// M-B + CR-2: per-leg cast in a composite FK. The text leg
    /// gets `::text` while the numeric leg stays on direct equality.
    #[test]
    fn composite_fk_per_leg_pg_cast() {
        let (sql, values) = build_fk_select_sql(
            r#""public"."order_items""#,
            &[
                leg(r#""order_id""#, Value::Int(99)),
                leg(r#""sku""#, Value::String("abc-123".into())),
            ],
            Dialect::Postgres,
        );
        assert_eq!(
            sql,
            r#"SELECT * FROM "public"."order_items" WHERE "order_id" = $1 AND "sku"::text = $2"#
        );
        assert_eq!(values.len(), 2);
    }

    /// m-1 regression: `Value::Unknown` is the driver's fallback
    /// for unmapped types and also binds as text on `PostgreSQL`.
    /// It needs the same `::text` cast as `Value::String` for the
    /// planner to accept the comparison against a non-text column.
    #[test]
    fn pg_unknown_value_casts_column_to_text() {
        let (sql, _) = build_fk_select_sql(
            r#""public"."users""#,
            &[leg(r#""id""#, Value::Unknown("opaque".into()))],
            Dialect::Postgres,
        );
        assert_eq!(
            sql,
            r#"SELECT * FROM "public"."users" WHERE "id"::text = $1"#
        );
    }

    /// PG native-typed values stay on the direct equality path so
    /// the FK index is usable. Uuid / Bool / Float / Json /
    /// Date / Bytes all bind at their native type.
    #[test]
    fn pg_native_typed_values_keep_direct_equality() {
        for v in [
            Value::Bool(true),
            Value::Float(1.5),
            Value::Bytes(vec![0, 1, 2]),
        ] {
            let (sql, _) = build_fk_select_sql(
                r#""public"."t""#,
                &[leg(r#""c""#, v.clone())],
                Dialect::Postgres,
            );
            assert!(
                !sql.contains("::text"),
                "value {v:?} should not trigger ::text cast (got {sql})"
            );
        }
    }

    /// `MySQL` composite FK uses `?` placeholders without casts.
    #[test]
    fn composite_fk_mysql_uses_question_marks() {
        let (sql, _) = build_fk_select_sql(
            "`shop`.`order_items`",
            &[
                leg("`order_id`", Value::Int(1)),
                leg("`sku`", Value::String("x".into())),
            ],
            Dialect::MySql,
        );
        assert_eq!(
            sql,
            "SELECT * FROM `shop`.`order_items` WHERE `order_id` = ? AND `sku` = ?"
        );
    }
}
