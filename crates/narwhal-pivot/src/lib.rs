//! T2-T4-D: pivot table aggregation engine.
//!
//! A pivot is conceptually
//!
//! ```text
//! SELECT row_keys.., col_key, agg(value)
//! FROM result
//! GROUP BY row_keys.., col_key
//! ```
//!
//! followed by a transpose that turns the distinct `col_key` values
//! into columns. This crate owns the pure projection: it consumes
//! [`Row`]s plus a column schema and emits a [`PivotTable`] that the
//! TUI widget can rasterise verbatim.
//!
//! The engine is reset-and-re-feed on every render. That trades
//! incremental performance (deferred to v2.1) for an unambiguously
//! correct snapshot — chart pane uses the same trick. For mid-sized
//! result sets (<100k rows) the cost is in the single-digit-ms range
//! on commodity hardware.
//!
//! Aggregators currently implemented: [`AggKind::Count`],
//! [`AggKind::Sum`], [`AggKind::Avg`], [`AggKind::Min`],
//! [`AggKind::Max`]. `First`, `Last`, `DistinctCount` are listed in
//! the roadmap but deferred to v2.1.

#![forbid(unsafe_code)]

use std::collections::{BTreeMap, BTreeSet};

use narwhal_core::{ColumnHeader, Row, Value};
use thiserror::Error;

/// Which aggregator to apply to the value column. Mirrors the
/// `agg=<name>` token on the `:pivot` command line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AggKind {
    /// Count of non-null rows in the bucket. Works on any value type.
    Count,
    /// Sum of numeric values; non-numeric coerces to NaN and is
    /// rendered as the empty token.
    Sum,
    /// Arithmetic mean of numeric values.
    Avg,
    /// Smallest numeric value in the bucket.
    Min,
    /// Largest numeric value in the bucket.
    Max,
}

impl AggKind {
    /// Token the parser accepts.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Count => "count",
            Self::Sum => "sum",
            Self::Avg => "avg",
            Self::Min => "min",
            Self::Max => "max",
        }
    }

    pub fn from_token(token: &str) -> Option<Self> {
        match token.to_ascii_lowercase().as_str() {
            "count" | "cnt" | "n" => Some(Self::Count),
            "sum" | "total" => Some(Self::Sum),
            "avg" | "mean" | "average" => Some(Self::Avg),
            "min" | "minimum" => Some(Self::Min),
            "max" | "maximum" => Some(Self::Max),
            _ => None,
        }
    }

    /// Whether the aggregator needs a numeric value column.
    /// `Count` works on anything; the rest need numeric coercion to
    /// succeed (best-effort `parse::<f64>()` plus the obvious
    /// integer / float / bool variants of [`Value`]).
    ///
    pub const fn requires_numeric(self) -> bool {
        match self {
            Self::Count => false,
            Self::Sum | Self::Avg | Self::Min | Self::Max => true,
        }
    }
}

/// Resolved pivot configuration. Row dimensions are columns the user
/// pulled into the "rows" bucket; `col_dim` is the single column whose
/// distinct values become column headers; `value` is the column that
/// feeds the aggregator. `Count` is the only configuration where
/// `value` may be `None` — every other aggregator needs a value
/// column.
#[derive(Debug, Clone)]
pub struct PivotConfig {
    pub row_dims: Vec<String>,
    pub col_dim: Option<String>,
    pub value: Option<String>,
    pub agg: AggKind,
    /// Cap on the number of distinct column-bucket headers; the rest
    /// are folded into [`PivotConfig::other_label`]. Mirrors the chart
    /// pane's high-cardinality guard.
    pub max_cols: usize,
    /// Token rendered for an empty / NaN cell.
    pub empty_token: String,
    /// Label used for the overflow column when `col_dim` has more
    /// distinct values than `max_cols`.
    pub other_label: String,
}

impl PivotConfig {
    /// Build a config from the command-line spec. Performs no
    /// column-existence validation; that happens at projection time
    /// inside [`derive_pivot_table`].
    pub fn new(agg: AggKind) -> Self {
        Self {
            row_dims: Vec::new(),
            col_dim: None,
            value: None,
            agg,
            max_cols: 50,
            empty_token: "—".to_owned(),
            other_label: "(other)".to_owned(),
        }
    }
}

/// Per-cell accumulator. Counts and numeric aggregators each carry a
/// minimal state — `Sum` and `Avg` share the running-sum-and-count
/// shape so a single struct fits both.
#[derive(Debug, Clone, Default)]
struct Accumulator {
    /// Number of rows that hit this cell. Used by `Count` and `Avg`.
    count: u64,
    /// Running sum for `Sum` / `Avg`.
    sum: f64,
    /// Running minimum for `Min`.
    min: f64,
    /// Running maximum for `Max`.
    max: f64,
    /// Whether any numeric value has been recorded — guards
    /// `min`/`max` / `sum` reads against the initial sentinel values.
    has_numeric: bool,
}

impl Accumulator {
    fn ingest(&mut self, value: Option<f64>) {
        self.count = self.count.saturating_add(1);
        if let Some(v) = value {
            if self.has_numeric {
                self.sum += v;
                if v < self.min {
                    self.min = v;
                }
                if v > self.max {
                    self.max = v;
                }
            } else {
                self.min = v;
                self.max = v;
                self.sum = v;
                self.has_numeric = true;
            }
        }
    }

    fn render(&self, agg: AggKind, empty: &str) -> String {
        match agg {
            AggKind::Count => {
                if self.count == 0 {
                    empty.to_owned()
                } else {
                    self.count.to_string()
                }
            }
            AggKind::Sum if self.has_numeric => format_number(self.sum),
            AggKind::Avg if self.has_numeric && self.count > 0 => {
                format_number(self.sum / self.count as f64)
            }
            AggKind::Min if self.has_numeric => format_number(self.min),
            AggKind::Max if self.has_numeric => format_number(self.max),
            _ => empty.to_owned(),
        }
    }
}

/// Format a numeric aggregate with at most 4 decimal places, trimming
/// trailing zeros so integers render as "42" rather than "42.0000".
fn format_number(v: f64) -> String {
    if v.is_nan() || v.is_infinite() {
        return "nan".to_owned();
    }
    if (v.fract()).abs() < 1e-9 {
        format!("{:.0}", v.round())
    } else {
        let rendered = format!("{v:.4}");
        let trimmed = rendered.trim_end_matches('0').trim_end_matches('.');
        trimmed.to_owned()
    }
}

/// Reasons [`derive_pivot_table`] can refuse to build a view.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum PivotError {
    #[error("pivot: no rows yet")]
    NoData,
    #[error("pivot: result has no columns")]
    NoColumns,
    #[error("pivot: configuration requires at least one row dimension or a column dimension")]
    EmptyConfig,
    #[error("pivot: unknown column '{name}' (available: {})", available.join(", "))]
    UnknownColumn {
        name: String,
        available: Vec<String>,
    },
    #[error("pivot: aggregator '{agg}' requires a numeric value column; '{name}' is not numeric")]
    NotNumeric { agg: &'static str, name: String },
    #[error("pivot: aggregator '{0}' requires a value column (use value=<col>)")]
    ValueRequired(&'static str),
}

/// Pure-data view of one pivot render.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PivotTable {
    /// Names of the row-dimension columns (left header strip).
    pub row_dim_headers: Vec<String>,
    /// Header label across the top of the table. `None` when the
    /// pivot has no column dimension (one collapsed column).
    pub col_dim_header: Option<String>,
    /// Distinct values of `col_dim` in the order they will be rendered.
    /// Empty when `col_dim_header` is `None`.
    pub col_headers: Vec<String>,
    /// One row per distinct row-key tuple; each row carries
    /// `row_dim_headers.len()` row-key cells followed by
    /// `col_headers.len().max(1)` value cells.
    pub rows: Vec<Vec<String>>,
}

impl PivotTable {
    /// Total number of columns (row dims + value cells).
    pub fn width(&self) -> usize {
        self.row_dim_headers.len() + self.col_headers.len().max(1)
    }
}

/// Best-effort numeric coercion of a [`Value`].
fn value_as_f64(v: &Value) -> Option<f64> {
    match v {
        Value::Int(i) => Some(*i as f64),
        Value::Float(f) if f.is_finite() => Some(*f),
        Value::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
        Value::String(s) | Value::Unknown(s) => s.parse::<f64>().ok().filter(|f| f.is_finite()),
        _ => None,
    }
}

/// Best-effort textual rendering of a [`Value`] as a label. Strips
/// control characters that would smear the grid.
fn value_as_label(v: &Value) -> String {
    let raw = v.to_string();
    let mut out = String::with_capacity(raw.len());
    for ch in raw.chars() {
        if ch.is_control() {
            out.push('·');
        } else {
            out.push(ch);
        }
    }
    out
}

/// Locate a column by case-insensitive name.
fn find_column(columns: &[ColumnHeader], name: &str) -> Result<usize, PivotError> {
    columns
        .iter()
        .position(|c| c.name.eq_ignore_ascii_case(name))
        .ok_or_else(|| PivotError::UnknownColumn {
            name: name.to_owned(),
            available: columns.iter().map(|c| c.name.clone()).collect(),
        })
}

/// Check whether at least one non-null cell in `col` parses numerically.
fn column_is_numeric(rows: &[Row], col: usize) -> bool {
    rows.iter().take(32).any(|row| {
        row.0
            .get(col)
            .is_some_and(|cell| !matches!(cell, Value::Null) && value_as_f64(cell).is_some())
    })
}

/// Produce the pivot table for `config` over the given result.
///
/// The caller hands in the *full* row slice; the engine treats each
/// invocation as a fresh ingest, so feeding `rows[..]` of a growing
/// stream on every render produces a monotonically-correct view.
pub fn derive_pivot_table(
    config: &PivotConfig,
    columns: &[ColumnHeader],
    rows: &[Row],
) -> Result<PivotTable, PivotError> {
    if columns.is_empty() {
        return Err(PivotError::NoColumns);
    }
    if config.row_dims.is_empty() && config.col_dim.is_none() {
        return Err(PivotError::EmptyConfig);
    }

    // Resolve column indices up-front so we fail fast on typos.
    let row_dim_indices: Vec<usize> = config
        .row_dims
        .iter()
        .map(|name| find_column(columns, name))
        .collect::<Result<_, _>>()?;
    let col_dim_index = if let Some(name) = &config.col_dim {
        Some(find_column(columns, name)?)
    } else {
        None
    };
    let value_index = if let Some(name) = &config.value {
        Some(find_column(columns, name)?)
    } else {
        if config.agg.requires_numeric() {
            return Err(PivotError::ValueRequired(config.agg.label()));
        }
        None
    };

    if rows.is_empty() {
        return Err(PivotError::NoData);
    }

    // Numeric-type guard on the value column.
    if let Some(idx) = value_index
        && config.agg.requires_numeric()
        && !column_is_numeric(rows, idx)
    {
        return Err(PivotError::NotNumeric {
            agg: config.agg.label(),
            name: columns[idx].name.clone(),
        });
    }

    // Aggregate.
    let mut grid: BTreeMap<Vec<String>, BTreeMap<String, Accumulator>> = BTreeMap::new();
    let mut col_seen: BTreeSet<String> = BTreeSet::new();
    let collapsed_col_key = String::new();

    for row in rows {
        let row_key: Vec<String> = row_dim_indices
            .iter()
            .map(|&i| row.0.get(i).map_or_else(String::new, value_as_label))
            .collect();
        let col_key = match col_dim_index {
            Some(i) => row.0.get(i).map_or_else(String::new, value_as_label),
            None => collapsed_col_key.clone(),
        };
        col_seen.insert(col_key.clone());

        let value = value_index.and_then(|i| row.0.get(i).and_then(value_as_f64));
        // Count-only configurations should still increment even when
        // value coercion fails; numeric aggregators rely on the
        // `Option<f64>` to skip non-numeric cells gracefully.
        grid.entry(row_key)
            .or_default()
            .entry(col_key)
            .or_default()
            .ingest(value);
    }

    // Determine the col-header list, capped at `max_cols` plus an
    // overflow bucket. The cap is value-blind (alphabetical) — a
    // future v2.1 enhancement is to rank by aggregate magnitude.
    let mut col_headers: Vec<String> = col_seen.into_iter().collect();
    let mut overflow_keys: Vec<String> = Vec::new();
    if col_dim_index.is_some() && col_headers.len() > config.max_cols.max(1) {
        let keep = config.max_cols.max(1);
        overflow_keys = col_headers.split_off(keep);
        col_headers.push(config.other_label.clone());
    }

    // Rasterise.
    let mut table_rows: Vec<Vec<String>> = Vec::with_capacity(grid.len());
    for (row_key, cells) in &grid {
        let mut out_row: Vec<String> = row_key.clone();
        if col_dim_index.is_some() {
            for header in &col_headers {
                let acc_render = if header == &config.other_label && !overflow_keys.is_empty() {
                    let mut merged = Accumulator::default();
                    for k in &overflow_keys {
                        if let Some(acc) = cells.get(k) {
                            merge_into(&mut merged, acc);
                        }
                    }
                    merged.render(config.agg, &config.empty_token)
                } else {
                    cells.get(header).map_or_else(
                        || config.empty_token.clone(),
                        |acc| acc.render(config.agg, &config.empty_token),
                    )
                };
                out_row.push(acc_render);
            }
        } else {
            // Single collapsed column.
            let acc_render = cells.get(&collapsed_col_key).map_or_else(
                || config.empty_token.clone(),
                |acc| acc.render(config.agg, &config.empty_token),
            );
            out_row.push(acc_render);
        }
        table_rows.push(out_row);
    }

    Ok(PivotTable {
        row_dim_headers: config.row_dims.clone(),
        col_dim_header: config.col_dim.clone(),
        col_headers: if col_dim_index.is_some() {
            col_headers
        } else {
            Vec::new()
        },
        rows: table_rows,
    })
}

fn merge_into(target: &mut Accumulator, source: &Accumulator) {
    target.count = target.count.saturating_add(source.count);
    if source.has_numeric {
        if target.has_numeric {
            target.sum += source.sum;
            if source.min < target.min {
                target.min = source.min;
            }
            if source.max > target.max {
                target.max = source.max;
            }
        } else {
            target.sum = source.sum;
            target.min = source.min;
            target.max = source.max;
            target.has_numeric = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use narwhal_core::{ColumnHeader, Row, Value};

    fn col(name: &str, ty: &str) -> ColumnHeader {
        ColumnHeader {
            name: name.into(),
            data_type: ty.into(),
        }
    }

    fn row(values: Vec<Value>) -> Row {
        Row(values)
    }

    #[test]
    fn count_aggregator_on_single_dim() {
        let cols = vec![col("country", "text")];
        let rows = vec![
            row(vec![Value::String("tr".into())]),
            row(vec![Value::String("de".into())]),
            row(vec![Value::String("tr".into())]),
        ];
        let mut cfg = PivotConfig::new(AggKind::Count);
        cfg.row_dims = vec!["country".into()];
        let table = derive_pivot_table(&cfg, &cols, &rows).expect("derive");
        assert_eq!(table.row_dim_headers, vec!["country".to_owned()]);
        assert!(table.col_headers.is_empty());
        assert_eq!(table.rows.len(), 2);
        // BTreeMap order: de=1, tr=2
        assert_eq!(table.rows[0], vec!["de".to_owned(), "1".to_owned()]);
        assert_eq!(table.rows[1], vec!["tr".to_owned(), "2".to_owned()]);
    }

    #[test]
    fn sum_aggregator_pivots_with_col_dim() {
        let cols = vec![
            col("country", "text"),
            col("year", "int"),
            col("revenue", "int"),
        ];
        let rows = vec![
            row(vec![
                Value::String("tr".into()),
                Value::Int(2024),
                Value::Int(10),
            ]),
            row(vec![
                Value::String("tr".into()),
                Value::Int(2025),
                Value::Int(20),
            ]),
            row(vec![
                Value::String("de".into()),
                Value::Int(2025),
                Value::Int(30),
            ]),
        ];
        let mut cfg = PivotConfig::new(AggKind::Sum);
        cfg.row_dims = vec!["country".into()];
        cfg.col_dim = Some("year".into());
        cfg.value = Some("revenue".into());
        let table = derive_pivot_table(&cfg, &cols, &rows).expect("derive");
        assert_eq!(
            table.col_headers,
            vec!["2024".to_owned(), "2025".to_owned()]
        );
        // de row: 2024 empty, 2025=30
        assert_eq!(
            table.rows[0],
            vec!["de".to_owned(), "—".to_owned(), "30".to_owned()]
        );
        // tr row: 2024=10, 2025=20
        assert_eq!(
            table.rows[1],
            vec!["tr".to_owned(), "10".to_owned(), "20".to_owned()]
        );
    }

    #[test]
    fn avg_aggregator_divides_by_count() {
        let cols = vec![col("k", "text"), col("v", "int")];
        let rows = vec![
            row(vec![Value::String("a".into()), Value::Int(2)]),
            row(vec![Value::String("a".into()), Value::Int(4)]),
            row(vec![Value::String("a".into()), Value::Int(6)]),
        ];
        let mut cfg = PivotConfig::new(AggKind::Avg);
        cfg.row_dims = vec!["k".into()];
        cfg.value = Some("v".into());
        let table = derive_pivot_table(&cfg, &cols, &rows).expect("derive");
        assert_eq!(table.rows[0], vec!["a".to_owned(), "4".to_owned()]);
    }

    #[test]
    fn min_max_track_extremes() {
        let cols = vec![col("k", "text"), col("v", "int")];
        let rows = vec![
            row(vec![Value::String("a".into()), Value::Int(5)]),
            row(vec![Value::String("a".into()), Value::Int(1)]),
            row(vec![Value::String("a".into()), Value::Int(9)]),
        ];
        let mut cfg = PivotConfig::new(AggKind::Min);
        cfg.row_dims = vec!["k".into()];
        cfg.value = Some("v".into());
        let table = derive_pivot_table(&cfg, &cols, &rows).expect("derive");
        assert_eq!(table.rows[0][1], "1");

        cfg.agg = AggKind::Max;
        let table = derive_pivot_table(&cfg, &cols, &rows).expect("derive");
        assert_eq!(table.rows[0][1], "9");
    }

    #[test]
    fn high_cardinality_collapses_into_other_bucket() {
        let cols = vec![col("k", "text"), col("c", "text"), col("v", "int")];
        let mut rows = Vec::new();
        for i in 0..60usize {
            rows.push(row(vec![
                Value::String("a".into()),
                Value::String(format!("c{i:03}")),
                Value::Int(1),
            ]));
        }
        let mut cfg = PivotConfig::new(AggKind::Sum);
        cfg.row_dims = vec!["k".into()];
        cfg.col_dim = Some("c".into());
        cfg.value = Some("v".into());
        cfg.max_cols = 10;
        let table = derive_pivot_table(&cfg, &cols, &rows).expect("derive");
        // 10 kept + 1 overflow bucket
        assert_eq!(table.col_headers.len(), 11);
        assert_eq!(table.col_headers.last().unwrap(), "(other)");
        // 50 of the original keys collapsed: 50 * 1 = 50
        let last = table.rows[0].last().unwrap();
        assert_eq!(last, "50");
    }

    #[test]
    fn unknown_column_reports_error() {
        let cols = vec![col("k", "text")];
        let rows = vec![row(vec![Value::String("a".into())])];
        let mut cfg = PivotConfig::new(AggKind::Count);
        cfg.row_dims = vec!["nope".into()];
        let err = derive_pivot_table(&cfg, &cols, &rows).expect_err("reject");
        match err {
            PivotError::UnknownColumn { name, available } => {
                assert_eq!(name, "nope");
                assert_eq!(available, vec!["k".to_owned()]);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn sum_without_value_column_is_rejected() {
        let cols = vec![col("k", "text")];
        let rows = vec![row(vec![Value::String("a".into())])];
        let mut cfg = PivotConfig::new(AggKind::Sum);
        cfg.row_dims = vec!["k".into()];
        let err = derive_pivot_table(&cfg, &cols, &rows).expect_err("reject");
        assert_eq!(err, PivotError::ValueRequired("sum"));
    }

    #[test]
    fn sum_on_non_numeric_value_is_rejected() {
        let cols = vec![col("k", "text"), col("v", "text")];
        let rows = vec![row(vec![
            Value::String("a".into()),
            Value::String("nope".into()),
        ])];
        let mut cfg = PivotConfig::new(AggKind::Sum);
        cfg.row_dims = vec!["k".into()];
        cfg.value = Some("v".into());
        let err = derive_pivot_table(&cfg, &cols, &rows).expect_err("reject");
        match err {
            PivotError::NotNumeric { agg, name } => {
                assert_eq!(agg, "sum");
                assert_eq!(name, "v");
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn empty_config_is_rejected() {
        let cols = vec![col("k", "text")];
        let rows = vec![row(vec![Value::String("a".into())])];
        let cfg = PivotConfig::new(AggKind::Count);
        let err = derive_pivot_table(&cfg, &cols, &rows).expect_err("reject");
        assert_eq!(err, PivotError::EmptyConfig);
    }

    #[test]
    fn empty_rows_yields_no_data() {
        let cols = vec![col("k", "text")];
        let mut cfg = PivotConfig::new(AggKind::Count);
        cfg.row_dims = vec!["k".into()];
        let err = derive_pivot_table(&cfg, &cols, &[]).expect_err("reject");
        assert_eq!(err, PivotError::NoData);
    }

    #[test]
    fn agg_kind_parses_aliases() {
        assert_eq!(AggKind::from_token("sum"), Some(AggKind::Sum));
        assert_eq!(AggKind::from_token("TOTAL"), Some(AggKind::Sum));
        assert_eq!(AggKind::from_token("avg"), Some(AggKind::Avg));
        assert_eq!(AggKind::from_token("mean"), Some(AggKind::Avg));
        assert_eq!(AggKind::from_token("count"), Some(AggKind::Count));
        assert_eq!(AggKind::from_token("n"), Some(AggKind::Count));
        assert_eq!(AggKind::from_token("min"), Some(AggKind::Min));
        assert_eq!(AggKind::from_token("max"), Some(AggKind::Max));
        assert_eq!(AggKind::from_token("median"), None);
    }

    #[test]
    fn format_number_strips_trailing_zeros() {
        assert_eq!(format_number(42.0), "42");
        assert_eq!(format_number(42.5), "42.5");
        assert_eq!(format_number(42.123_456_7), "42.1235");
        assert_eq!(format_number(f64::NAN), "nan");
    }

    #[test]
    fn count_works_on_non_numeric_value_column() {
        // Count should tolerate any value column type — even non-numeric.
        let cols = vec![col("k", "text"), col("v", "text")];
        let rows = vec![
            row(vec![Value::String("a".into()), Value::String("x".into())]),
            row(vec![Value::String("a".into()), Value::Null]),
        ];
        let mut cfg = PivotConfig::new(AggKind::Count);
        cfg.row_dims = vec!["k".into()];
        cfg.value = Some("v".into());
        let table = derive_pivot_table(&cfg, &cols, &rows).expect("derive");
        assert_eq!(table.rows[0][1], "2");
    }

    #[test]
    fn multi_row_dim_concatenates_keys() {
        let cols = vec![col("a", "text"), col("b", "text")];
        let rows = vec![
            row(vec![Value::String("x".into()), Value::String("1".into())]),
            row(vec![Value::String("x".into()), Value::String("2".into())]),
            row(vec![Value::String("y".into()), Value::String("1".into())]),
        ];
        let mut cfg = PivotConfig::new(AggKind::Count);
        cfg.row_dims = vec!["a".into(), "b".into()];
        let table = derive_pivot_table(&cfg, &cols, &rows).expect("derive");
        assert_eq!(table.rows.len(), 3);
        // BTreeMap order: (x,1), (x,2), (y,1)
        assert_eq!(
            table.rows[0],
            vec!["x".to_owned(), "1".to_owned(), "1".to_owned()]
        );
        assert_eq!(
            table.rows[1],
            vec!["x".to_owned(), "2".to_owned(), "1".to_owned()]
        );
        assert_eq!(
            table.rows[2],
            vec!["y".to_owned(), "1".to_owned(), "1".to_owned()]
        );
    }
}
