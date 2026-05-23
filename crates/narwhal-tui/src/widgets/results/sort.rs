//! Sort direction and value comparison.

use std::cmp::Ordering;

use narwhal_core::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
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

pub(super) const fn type_rank(v: &Value) -> u8 {
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

pub(super) fn compare_same_type(a: &Value, b: &Value) -> Ordering {
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
pub(super) fn compare_json(a: &serde_json::Value, b: &serde_json::Value) -> Ordering {
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

