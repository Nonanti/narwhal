//! Pure result-pane value types.
//!
//! These describe *what* the result pane shows; they do not carry any
//! ratatui state. Hosts (the TUI widget crate) consume them by
//! reference. Mutations are routed through plain field access or the
//! small set of inherent methods below.
//!
//! Moved out of `narwhal-tui::widgets::results` so the TUI no longer
//! owns shared domain types. The TUI re-exports each item to keep the
//! existing `narwhal_tui::MetaTab` / `narwhal_tui::SortDir` import
//! paths working.

use std::cmp::Ordering;

use narwhal_core::{ColumnHeader, Row, Value};

// ---------------------------------------------------------------------
// MetaTab
// ---------------------------------------------------------------------

/// Which metadata sub-view of the table-detail pane is on screen.
/// Mapped 1:1 from the numeric chord (`1`..=`5`) on the Results pane
/// and round-trips through [`MetaTab::index`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MetaTab {
    /// `1`: the row preview (paged `SELECT *`). Selecting this tab
    /// from any other dispatches a preview query against the table;
    /// the active state becomes the row-display view until the user
    /// navigates back.
    Records,
    /// `2`: columns table with type / nullability / PK / default.
    #[default]
    Columns,
    /// `3`: primary key + unique constraints.
    Constraints,
    /// `4`: foreign keys with ON UPDATE/ON DELETE actions.
    ForeignKeys,
    /// `5`: secondary indexes.
    Indexes,
}

impl MetaTab {
    /// 1-based display index used both in the tab strip and as the
    /// numeric keybinding (`1` selects `Records`, etc.).
    pub const fn index(self) -> u8 {
        match self {
            Self::Records => 1,
            Self::Columns => 2,
            Self::Constraints => 3,
            Self::ForeignKeys => 4,
            Self::Indexes => 5,
        }
    }

    /// Inverse of [`Self::index`]; `None` for out-of-range inputs so
    /// future chord additions can grow without panicking.
    pub const fn from_index(n: u8) -> Option<Self> {
        match n {
            1 => Some(Self::Records),
            2 => Some(Self::Columns),
            3 => Some(Self::Constraints),
            4 => Some(Self::ForeignKeys),
            5 => Some(Self::Indexes),
            _ => None,
        }
    }

    /// Short label shown in the tab strip. Stays ASCII so the renderer
    /// width math (one column per character) keeps working in TTYs
    /// that lack wide-glyph support.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Records => "Records",
            Self::Columns => "Columns",
            Self::Constraints => "Constraints",
            Self::ForeignKeys => "FKs",
            Self::Indexes => "Indexes",
        }
    }

    /// All variants in display order. Iterating is preferred over
    /// hand-rolled lists so a new variant lights up everywhere at
    /// once.
    pub const fn all() -> &'static [Self] {
        &[
            Self::Records,
            Self::Columns,
            Self::Constraints,
            Self::ForeignKeys,
            Self::Indexes,
        ]
    }
}

// ---------------------------------------------------------------------
// SortDir + value comparison
// ---------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortDir {
    Asc,
    Desc,
}

/// Compare two optional [`Value`] references for sorting purposes.
///
/// Ordering rules:
/// - `None` (missing column) sorts last regardless of direction.
/// - `Null` sorts last in Asc, first in Desc.
/// - Same-type values compare naturally (Int numerically, String
///   lexicographically, etc.).
/// - Different types sort by a stable type-order: Int < Float < Bool <
///   String < Bytes < Date < Time < `DateTime` < Timestamp < Uuid < Json <
///   Unknown.
pub fn compare_values(a: Option<&Value>, b: Option<&Value>) -> Ordering {
    match (a, b) {
        (None, None) => Ordering::Equal,
        (None, _) => Ordering::Greater,
        (_, None) => Ordering::Less,
        (Some(Value::Null), Some(Value::Null)) => Ordering::Equal,
        (Some(Value::Null), _) => Ordering::Greater,
        (_, Some(Value::Null)) => Ordering::Less,
        (Some(va), Some(vb)) => {
            let ta = type_rank(va);
            let tb = type_rank(vb);
            match ta.cmp(&tb) {
                Ordering::Equal => compare_same_type(va, vb),
                other => other,
            }
        }
    }
}

const fn type_rank(v: &Value) -> u8 {
    match v {
        Value::Int(_) => 0,
        Value::Float(_) => 1,
        Value::Bool(_) => 2,
        Value::String(_) => 3,
        Value::Bytes(_) => 4,
        Value::Date(_) => 5,
        Value::Time(_) => 6,
        Value::DateTime(_) => 7,
        Value::Timestamp(_) => 8,
        Value::Uuid(_) => 9,
        Value::Json(_) => 10,
        Value::Unknown(_) => 11,
        Value::Null => 12, // unreachable in practice but included for completeness
        // Future variants get sorted after Null until ranked explicitly.
        _ => 13,
    }
}

fn compare_same_type(a: &Value, b: &Value) -> Ordering {
    match (a, b) {
        (Value::Int(x), Value::Int(y)) => x.cmp(y),
        (Value::Float(x), Value::Float(y)) => x.partial_cmp(y).unwrap_or(Ordering::Equal),
        (Value::Bool(x), Value::Bool(y)) => x.cmp(y),
        (Value::String(x), Value::String(y)) => x.cmp(y),
        (Value::Bytes(x), Value::Bytes(y)) => x.cmp(y),
        (Value::Date(x), Value::Date(y)) => x.cmp(y),
        (Value::Time(x), Value::Time(y)) => x.cmp(y),
        (Value::DateTime(x), Value::DateTime(y)) => x.cmp(y),
        (Value::Timestamp(x), Value::Timestamp(y)) => x.cmp(y),
        (Value::Uuid(x), Value::Uuid(y)) => x.cmp(y),
        (Value::Json(x), Value::Json(y)) => compare_json(x, y),
        (Value::Unknown(x), Value::Unknown(y)) => x.cmp(y),
        _ => Ordering::Equal,
    }
}

/// Structurally compare two `serde_json::Value`s without materialising
/// either side via `to_string()`. The ordering is total and stable —
/// suitable for `sort_by` — but is deliberately *not* the same lexical
/// order `to_string()` produces; for sort UX purposes that doesn't
/// matter, what matters is that equal inputs compare equal and that
/// the result is deterministic across runs.
///
/// Performance: this allocates only when the operands are strings
/// (already-allocated `&str`) and never for numeric / bool / null /
/// nested-container leaves. Array compare is element-wise; object
/// compare iterates `serde_json::Map`'s in-order entries which are
/// already sorted when the feature is `preserve_order`-off (default).
fn compare_json(a: &serde_json::Value, b: &serde_json::Value) -> Ordering {
    use serde_json::Value as J;
    const fn rank(v: &J) -> u8 {
        match v {
            J::Null => 0,
            J::Bool(_) => 1,
            J::Number(_) => 2,
            J::String(_) => 3,
            J::Array(_) => 4,
            J::Object(_) => 5,
        }
    }
    match (a, b) {
        (J::Null, J::Null) => Ordering::Equal,
        (J::Bool(x), J::Bool(y)) => x.cmp(y),
        (J::Number(x), J::Number(y)) => {
            // serde_json::Number doesn't implement Ord because of NaN /
            // mixed int-vs-float semantics; fall back to f64 with the
            // partial_cmp -> Equal collapse the rest of the comparator
            // already uses for floats.
            match (x.as_f64(), y.as_f64()) {
                (Some(xf), Some(yf)) => xf.partial_cmp(&yf).unwrap_or(Ordering::Equal),
                _ => Ordering::Equal,
            }
        }
        (J::String(x), J::String(y)) => x.cmp(y),
        (J::Array(x), J::Array(y)) => {
            for (xa, yb) in x.iter().zip(y.iter()) {
                match compare_json(xa, yb) {
                    Ordering::Equal => {}
                    other => return other,
                }
            }
            x.len().cmp(&y.len())
        }
        (J::Object(x), J::Object(y)) => {
            for ((kx, vx), (ky, vy)) in x.iter().zip(y.iter()) {
                match kx.cmp(ky) {
                    Ordering::Equal => {}
                    other => return other,
                }
                match compare_json(vx, vy) {
                    Ordering::Equal => {}
                    other => return other,
                }
            }
            x.len().cmp(&y.len())
        }
        // Different JSON kinds — fall back to the type rank.
        _ => rank(a).cmp(&rank(b)),
    }
}

// ---------------------------------------------------------------------
// Cell popup / inline editor
// ---------------------------------------------------------------------

/// Modal description of one cell, shown over the result grid when the
/// user requests detail with Enter.
#[derive(Debug, Clone)]
pub struct CellPopup {
    pub column_name: String,
    pub column_type: String,
    pub value_text: String,
    pub row_index: usize,
}

/// Editor-style popup used by inline cell edits.
#[derive(Debug, Clone)]
pub struct CellEditView {
    pub column_name: String,
    pub column_type: String,
    pub row_index: usize,
    /// Current buffer the user is editing.
    pub buffer: String,
    /// Optional error message rendered below the input (e.g. UPDATE
    /// rejected by the engine).
    pub error: Option<String>,
}

// ---------------------------------------------------------------------
// ResultView
// ---------------------------------------------------------------------

/// Pure data half of the result-pane view. Carries every piece of
/// state the result grid needs *except* the ratatui `TableState` that
/// the renderer briefly materialises each frame.
///
/// The selection / scroll offset that ratatui drives at render time
/// are persisted here as plain fields so the TUI can rebuild a
/// `TableState` from them, hand it to `render_stateful_widget`, then
/// copy the (possibly updated) values back — see the renderer in
/// `narwhal-tui::widgets::results::table_paint` for the round-trip.
#[derive(Debug, Default)]
pub struct ResultView {
    /// Index of the selected row (post-filter/sort), if any. Mirrors
    /// `ratatui::widgets::TableState::selected`.
    pub selected: Option<usize>,
    /// Vertical scroll offset (post-filter/sort). Mirrors
    /// `ratatui::widgets::TableState::offset`.
    pub scroll_offset: usize,
    pub column_index: usize,
    pub popup: Option<CellPopup>,
    /// When `Some`, the cell editor is drawn on top of the result grid in
    /// place of the read-only popup. Only one of `popup` and `edit` is
    /// rendered at a time; the host app enforces this.
    pub edit: Option<CellEditView>,
    /// Active sort: `(column_index, direction)`.
    pub sort: Option<(usize, SortDir)>,
    /// Active filter text. Rows that don't contain this
    /// case-insensitive substring in any column are hidden.
    pub filter: String,
    /// When `true`, the filter input prompt is open for editing.
    pub filter_prompt_open: bool,
    /// Cached visible row indices computed by the last render.
    /// `visible_indices[i]` is the original row index of the i-th
    /// rendered row.
    pub visible_indices: Vec<usize>,
}

impl ResultView {
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the index of the selected row, or `None` when no row is
    /// selected. Mirrors `ratatui::widgets::TableState::selected`.
    pub const fn selected(&self) -> Option<usize> {
        self.selected
    }

    /// Select the row at `index`, or pass `None` to clear the
    /// selection.
    pub const fn select(&mut self, index: Option<usize>) {
        self.selected = index;
    }

    /// Vertical scroll offset.
    pub const fn scroll_offset(&self) -> usize {
        self.scroll_offset
    }

    /// Set the vertical scroll offset.
    pub const fn set_scroll_offset(&mut self, offset: usize) {
        self.scroll_offset = offset;
    }

    pub const fn move_down(&mut self, total_rows: usize) {
        if total_rows == 0 {
            return;
        }
        let next = match self.selected {
            Some(i) => i + 1,
            None => 0,
        };
        let max = total_rows - 1;
        self.selected = Some(if next < max { next } else { max });
    }

    pub const fn move_up(&mut self) {
        match self.selected {
            Some(i) => self.selected = Some(i.saturating_sub(1)),
            None => self.selected = Some(0),
        }
    }

    pub const fn move_left(&mut self) {
        self.column_index = self.column_index.saturating_sub(1);
    }

    pub const fn move_right(&mut self, total_cols: usize) {
        if total_cols == 0 {
            return;
        }
        if self.column_index + 1 < total_cols {
            self.column_index += 1;
        }
    }

    pub fn reset(&mut self) {
        self.selected = None;
        self.column_index = 0;
        self.popup = None;
        self.sort = None;
        self.filter.clear();
        self.filter_prompt_open = false;
        self.visible_indices.clear();
    }

    /// Derive the visible row indices after applying filter then sort.
    /// Filter applies first; sort applies to the filtered subset.
    /// Sort is stable across ties.
    pub fn visible_rows(&self, columns: &[ColumnHeader], rows: &[Row]) -> Vec<usize> {
        let mut indices: Vec<usize> = (0..rows.len()).collect();

        // Filter: keep rows where any cell contains the needle
        // (case-insensitive).
        if !self.filter.is_empty() {
            let needle = self.filter.to_lowercase();
            indices.retain(|&i| {
                rows[i]
                    .0
                    .iter()
                    .any(|v| v.render().to_lowercase().contains(&needle))
            });
        }

        // Sort: stable sort on the filtered subset.
        if let Some((col, dir)) = self.sort {
            let col_clamped = if col < columns.len() {
                col
            } else {
                return indices;
            };
            indices.sort_by(|&a, &b| {
                let av = rows[a].0.get(col_clamped);
                let bv = rows[b].0.get(col_clamped);
                let ord = compare_values(av, bv);
                match dir {
                    SortDir::Asc => ord,
                    SortDir::Desc => ord.reverse(),
                }
            });
        }

        indices
    }
}

// ---------------------------------------------------------------------
// ExplainPlanLine
// ---------------------------------------------------------------------

/// One rendered line of a query plan. Independent of the parser so the
/// widget crate does not need a dependency on `serde_json`.
///
/// v1.1 #3: extended with optional cost / divergence metadata so the
/// renderer can draw cost bars and colour the hot path. The fields
/// default to inert values so callers that haven't migrated still
/// produce a sensible monochrome plan.
#[derive(Debug, Clone, Default)]
pub struct ExplainPlanLine {
    pub depth: usize,
    pub text: String,
    /// Total cost of this node normalised to the plan's max cost
    /// (0.0–1.0). Drives the cost-bar fill width. `None` suppresses
    /// the bar entirely.
    pub cost_ratio: Option<f64>,
    /// `true` when the node is on the plan's hot path (highest cost
    /// branch from root to a leaf). Drawn with the accent colour.
    pub hot: bool,
    /// `true` when the actual rows diverge from the planner estimate
    /// by more than 10×. Drawn with a yellow badge.
    pub divergent: bool,
    /// Tree connector for this line, e.g. `"  ├─ "` / `"  └─ "`. When
    /// non-empty it is rendered verbatim *instead of* the indent +
    /// glyph the renderer used to compute itself, so callers can
    /// produce a proper box-drawing tree.
    pub connector: String,
}

#[cfg(test)]
mod tests {
    use std::cmp::Ordering;

    use narwhal_core::Value;

    use super::*;

    #[test]
    fn compare_json_structural_matches_string_for_equal_inputs() {
        let a = serde_json::json!({"id": 1, "tags": ["a", "b"]});
        let b = serde_json::json!({"id": 1, "tags": ["a", "b"]});
        assert_eq!(
            compare_values(Some(&Value::Json(a)), Some(&Value::Json(b))),
            Ordering::Equal
        );
    }

    #[test]
    fn compare_json_orders_by_first_differing_field() {
        let a = serde_json::json!({"name": "alice"});
        let b = serde_json::json!({"name": "bob"});
        assert_eq!(
            compare_values(Some(&Value::Json(a)), Some(&Value::Json(b))),
            Ordering::Less
        );
    }

    #[test]
    fn compare_json_orders_numbers_numerically_not_lexically() {
        // String-based compare would put "10" before "2"; structural
        // compare orders 2 < 10.
        let a = serde_json::json!(2);
        let b = serde_json::json!(10);
        assert_eq!(
            compare_values(Some(&Value::Json(a)), Some(&Value::Json(b))),
            Ordering::Less
        );
    }

    #[test]
    fn compare_json_arrays_use_lexicographic_order() {
        let a = serde_json::json!([1, 2, 3]);
        let b = serde_json::json!([1, 2, 4]);
        let c = serde_json::json!([1, 2]);
        assert_eq!(
            compare_values(Some(&Value::Json(a.clone())), Some(&Value::Json(b))),
            Ordering::Less
        );
        assert_eq!(
            compare_values(Some(&Value::Json(c)), Some(&Value::Json(a))),
            Ordering::Less
        );
    }

    #[test]
    fn compare_json_different_kinds_use_type_rank() {
        // bool ranks below string, regardless of payload.
        let a = serde_json::json!(true);
        let b = serde_json::json!("a");
        assert_eq!(
            compare_values(Some(&Value::Json(a)), Some(&Value::Json(b))),
            Ordering::Less
        );
    }
}
