//! T2-T4-C: inline ASCII chart state and data derivation.
//!
//! Chart configuration ([`ChartConfig`]) is sticky per-tab — once the
//! user types `:chart bar`, every subsequent run derives a fresh
//! [`ChartData`] from the active result's columns + rows. The data is
//! recomputed on every render so the "streaming" cadence is automatic:
//! as `RowsAppended` mutates the underlying `Vec<Row>` the chart picks
//! up the new points on the next frame without any side-channel
//! plumbing.
//!
//! Role auto-detection lives here (`detect_x_col`, `detect_y_col`); the
//! TUI widget consumes a pure data view (`ChartDisplay`) and knows
//! nothing about `narwhal-core` types.

use narwhal_core::{ColumnHeader, Row, Value};

/// Which ratatui widget the chart pane renders.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ChartKind {
    Bar,
    Line,
    Sparkline,
}

impl ChartKind {
    /// Token accepted on the `:chart` command line.
    pub(super) const fn label(self) -> &'static str {
        match self {
            Self::Bar => "bar",
            Self::Line => "line",
            Self::Sparkline => "sparkline",
        }
    }

    /// Default cap for the data buffer. Bars get a small cap because the
    /// chart visually smears past ~50 bars on a typical terminal; lines
    /// and sparklines can soak much more data.
    pub(super) const fn default_bound(self) -> usize {
        match self {
            Self::Bar => 50,
            Self::Line => 1_000,
            Self::Sparkline => 1_000,
        }
    }
}

/// Sticky chart configuration attached to a tab. Resolved column
/// overrides are stored by name (string) so the binding survives a
/// query change as long as the columns still exist.
#[derive(Debug, Clone)]
pub(crate) struct ChartConfig {
    pub(super) kind: ChartKind,
    pub(super) title: Option<String>,
    /// Optional `--x col` override. When `None` the renderer auto-picks
    /// the first non-numeric column for [`ChartKind::Bar`] / the row
    /// index for [`ChartKind::Line`].
    pub(super) x_col: Option<String>,
    /// Optional `--y col` (or `--col` for sparkline) override. When
    /// `None` the renderer picks the first numeric column.
    pub(super) y_col: Option<String>,
    /// Cap on the data buffer. Overflow is dropped from the front for
    /// line / sparkline (time-series view) and from the bottom by
    /// magnitude for bar charts (top-N).
    pub(super) bounded_to: usize,
}

impl ChartConfig {
    pub(super) const fn new(kind: ChartKind) -> Self {
        Self {
            kind,
            title: None,
            x_col: None,
            y_col: None,
            bounded_to: kind.default_bound(),
        }
    }
}

/// Reasons [`derive_chart_data`] can refuse to build a view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ChartError {
    /// No rows have arrived yet — caller renders a "no data yet" hint.
    NoData,
    /// The columns header was empty (e.g. an `Affected` result snuck
    /// in). Caller should display the underlying result instead.
    NoColumns,
    /// User picked an x column that doesn't exist on the active result.
    UnknownColumn {
        name: String,
        available: Vec<String>,
    },
    /// The numeric y column auto-detection failed because no column in
    /// the result is numeric.
    NoNumericColumn { available: Vec<String> },
    /// The user-specified y column isn't numeric.
    NotNumeric { name: String },
}

impl ChartError {
    /// Human-readable error suitable for the status bar.
    pub(super) fn message(&self) -> String {
        match self {
            Self::NoData => "chart: no data yet".to_owned(),
            Self::NoColumns => "chart: result has no columns".to_owned(),
            Self::UnknownColumn { name, available } => {
                format!(
                    "chart: unknown column '{name}' (available: {})",
                    available.join(", ")
                )
            }
            Self::NoNumericColumn { available } => format!(
                "chart: no numeric column found (available: {})",
                available.join(", ")
            ),
            Self::NotNumeric { name } => {
                format!("chart: column '{name}' is not numeric")
            }
        }
    }
}

/// Pure-data view of one chart render. The TUI widget builds its
/// ratatui drawables from this struct alone — no `narwhal-core`
/// dependency leaks across the crate boundary.
#[derive(Debug, Clone)]
pub(super) struct ChartData {
    pub(super) kind: ChartKind,
    pub(super) title: String,
    /// Bar / line: one label per point. Sparkline: empty.
    pub(super) labels: Vec<String>,
    /// Numeric values; same length as `labels` for bar / line.
    pub(super) values: Vec<f64>,
}

/// Best-effort numeric coercion of a [`Value`]. Returns `None` for
/// non-numeric variants so role-detection can skip past string columns.
pub(super) fn value_as_f64(v: &Value) -> Option<f64> {
    match v {
        Value::Int(i) => Some(*i as f64),
        Value::Float(f) if f.is_finite() => Some(*f),
        Value::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
        Value::String(s) | Value::Unknown(s) => s.parse::<f64>().ok().filter(|f| f.is_finite()),
        _ => None,
    }
}

/// Best-effort textual rendering of a [`Value`] as a chart label.
/// Strips control / wide-cell characters that would smear the chart.
pub(super) fn value_as_label(v: &Value) -> String {
    let raw = v.render();
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
pub(super) fn find_column(columns: &[ColumnHeader], name: &str) -> Option<usize> {
    columns
        .iter()
        .position(|c| c.name.eq_ignore_ascii_case(name))
}

/// Auto-detect the first numeric column. Sampling: peek the first
/// non-null cell in each column until a numeric is found.
pub(super) fn detect_numeric_column(columns: &[ColumnHeader], rows: &[Row]) -> Option<usize> {
    (0..columns.len()).find(|&i| column_is_numeric(rows, i))
}

/// Auto-detect the first non-numeric column. Used as the x-axis for
/// bar charts where the labels should be categorical.
pub(super) fn detect_categorical_column(columns: &[ColumnHeader], rows: &[Row]) -> Option<usize> {
    (0..columns.len()).find(|&i| !column_is_numeric(rows, i))
}

fn column_is_numeric(rows: &[Row], col: usize) -> bool {
    for row in rows.iter().take(32) {
        let Some(cell) = row.0.get(col) else { continue };
        if matches!(cell, Value::Null) {
            continue;
        }
        return value_as_f64(cell).is_some();
    }
    false
}

/// Derive a [`ChartData`] from a configuration and the active result's
/// columns + rows. Pure; safe to call on every frame.
pub(super) fn derive_chart_data(
    config: &ChartConfig,
    columns: &[ColumnHeader],
    rows: &[Row],
) -> Result<ChartData, ChartError> {
    if columns.is_empty() {
        return Err(ChartError::NoColumns);
    }
    if rows.is_empty() {
        return Err(ChartError::NoData);
    }
    let available: Vec<String> = columns.iter().map(|c| c.name.clone()).collect();

    // Resolve y column.
    let y_col = match &config.y_col {
        Some(name) => {
            let idx = find_column(columns, name).ok_or_else(|| ChartError::UnknownColumn {
                name: name.clone(),
                available: available.clone(),
            })?;
            if !column_is_numeric(rows, idx) {
                return Err(ChartError::NotNumeric { name: name.clone() });
            }
            idx
        }
        None => {
            detect_numeric_column(columns, rows).ok_or_else(|| ChartError::NoNumericColumn {
                available: available.clone(),
            })?
        }
    };

    // Resolve x column (bar / line only).
    let x_col_idx = match config.kind {
        ChartKind::Sparkline => None,
        ChartKind::Bar | ChartKind::Line => match &config.x_col {
            Some(name) => {
                Some(
                    find_column(columns, name).ok_or_else(|| ChartError::UnknownColumn {
                        name: name.clone(),
                        available: available.clone(),
                    })?,
                )
            }
            None => match config.kind {
                ChartKind::Bar => detect_categorical_column(columns, rows),
                ChartKind::Line => None, // line uses row index by default
                ChartKind::Sparkline => unreachable!(),
            },
        },
    };

    // Extract (label, value) pairs from each row.
    let mut points: Vec<(String, f64)> = Vec::with_capacity(rows.len().min(config.bounded_to * 4));
    for (idx, row) in rows.iter().enumerate() {
        let Some(cell) = row.0.get(y_col) else {
            continue;
        };
        let Some(v) = value_as_f64(cell) else {
            continue;
        };
        let label = match x_col_idx {
            Some(x) => row
                .0
                .get(x)
                .map_or_else(|| (idx + 1).to_string(), value_as_label),
            None => (idx + 1).to_string(),
        };
        points.push((label, v));
    }

    if points.is_empty() {
        return Err(ChartError::NoData);
    }

    // Bound the buffer. Bar = top-N by magnitude (so the chart stays
    // readable on high-cardinality columns); line / sparkline = tail
    // (time-series semantics).
    let bound = config.bounded_to.max(1);
    if points.len() > bound {
        match config.kind {
            ChartKind::Bar => {
                points.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
                points.truncate(bound);
            }
            ChartKind::Line | ChartKind::Sparkline => {
                let skip = points.len() - bound;
                points.drain(0..skip);
            }
        }
    }

    let title = config
        .title
        .clone()
        .unwrap_or_else(|| default_title(config, columns, x_col_idx, y_col));

    let (labels, values) = if config.kind == ChartKind::Sparkline {
        // Sparkline doesn't render labels; keep them empty so the
        // widget treats `values.len()` as the canonical length.
        (Vec::new(), points.into_iter().map(|(_, v)| v).collect())
    } else {
        let (labs, vals): (Vec<String>, Vec<f64>) = points.into_iter().unzip();
        (labs, vals)
    };

    Ok(ChartData {
        kind: config.kind,
        title,
        labels,
        values,
    })
}

fn default_title(
    config: &ChartConfig,
    columns: &[ColumnHeader],
    x: Option<usize>,
    y: usize,
) -> String {
    let y_name = columns.get(y).map_or("<unknown>", |c| c.name.as_str());
    match config.kind {
        ChartKind::Sparkline => format!("sparkline · {y_name}"),
        ChartKind::Bar | ChartKind::Line => match x.and_then(|i| columns.get(i)) {
            Some(xc) => format!("{} · {} vs {}", config.kind.label(), y_name, xc.name),
            None => format!("{} · {}", config.kind.label(), y_name),
        },
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
    fn value_coercion_handles_int_float_string() {
        assert_eq!(value_as_f64(&Value::Int(42)), Some(42.0));
        assert_eq!(value_as_f64(&Value::Float(3.5)), Some(3.5));
        assert_eq!(value_as_f64(&Value::String("7.5".into())), Some(7.5));
        assert_eq!(value_as_f64(&Value::String("nope".into())), None);
        assert_eq!(value_as_f64(&Value::Null), None);
        assert_eq!(value_as_f64(&Value::Bool(true)), Some(1.0));
    }

    #[test]
    fn float_nan_and_infinity_are_rejected() {
        assert_eq!(value_as_f64(&Value::Float(f64::NAN)), None);
        assert_eq!(value_as_f64(&Value::Float(f64::INFINITY)), None);
    }

    #[test]
    fn detect_first_numeric_column() {
        let cols = vec![col("name", "text"), col("count", "int")];
        let rows = vec![row(vec![Value::String("a".into()), Value::Int(1)])];
        assert_eq!(detect_numeric_column(&cols, &rows), Some(1));
        assert_eq!(detect_categorical_column(&cols, &rows), Some(0));
    }

    #[test]
    fn detect_skips_null_rows_when_sampling() {
        let cols = vec![col("v", "int")];
        let rows = vec![
            row(vec![Value::Null]),
            row(vec![Value::Null]),
            row(vec![Value::Int(7)]),
        ];
        assert_eq!(detect_numeric_column(&cols, &rows), Some(0));
    }

    #[test]
    fn bar_chart_auto_picks_categorical_x_numeric_y() {
        let cols = vec![col("country", "text"), col("revenue", "int")];
        let rows = vec![
            row(vec![Value::String("tr".into()), Value::Int(10)]),
            row(vec![Value::String("de".into()), Value::Int(30)]),
            row(vec![Value::String("us".into()), Value::Int(20)]),
        ];
        let data =
            derive_chart_data(&ChartConfig::new(ChartKind::Bar), &cols, &rows).expect("derive");
        assert_eq!(data.labels, vec!["tr", "de", "us"]);
        assert_eq!(data.values, vec![10.0, 30.0, 20.0]);
        assert!(data.title.contains("revenue"));
    }

    #[test]
    fn bar_chart_truncates_to_top_n_by_magnitude() {
        let cols = vec![col("k", "text"), col("v", "int")];
        let mut rows = Vec::new();
        for i in 0..120i64 {
            rows.push(row(vec![Value::String(format!("k{i}")), Value::Int(i)]));
        }
        let mut cfg = ChartConfig::new(ChartKind::Bar);
        cfg.bounded_to = 5;
        let data = derive_chart_data(&cfg, &cols, &rows).expect("derive");
        assert_eq!(data.values, vec![119.0, 118.0, 117.0, 116.0, 115.0]);
    }

    #[test]
    fn line_chart_tails_when_overflowing() {
        let cols = vec![col("v", "int")];
        let mut rows = Vec::new();
        for i in 0..120i64 {
            rows.push(row(vec![Value::Int(i)]));
        }
        let mut cfg = ChartConfig::new(ChartKind::Line);
        cfg.bounded_to = 4;
        let data = derive_chart_data(&cfg, &cols, &rows).expect("derive");
        assert_eq!(data.values, vec![116.0, 117.0, 118.0, 119.0]);
    }

    #[test]
    fn sparkline_yields_values_only() {
        let cols = vec![col("v", "int")];
        let rows = vec![row(vec![Value::Int(1)]), row(vec![Value::Int(3)])];
        let data = derive_chart_data(&ChartConfig::new(ChartKind::Sparkline), &cols, &rows)
            .expect("derive");
        assert!(data.labels.is_empty());
        assert_eq!(data.values, vec![1.0, 3.0]);
    }

    #[test]
    fn explicit_y_column_must_be_numeric() {
        let cols = vec![col("name", "text"), col("count", "int")];
        let rows = vec![row(vec![Value::String("a".into()), Value::Int(1)])];
        let mut cfg = ChartConfig::new(ChartKind::Bar);
        cfg.y_col = Some("name".into());
        let err = derive_chart_data(&cfg, &cols, &rows).expect_err("must reject");
        assert_eq!(
            err,
            ChartError::NotNumeric {
                name: "name".into()
            }
        );
    }

    #[test]
    fn unknown_column_reports_available() {
        let cols = vec![col("a", "int")];
        let rows = vec![row(vec![Value::Int(1)])];
        let mut cfg = ChartConfig::new(ChartKind::Bar);
        cfg.x_col = Some("nope".into());
        let err = derive_chart_data(&cfg, &cols, &rows).expect_err("must reject");
        match err {
            ChartError::UnknownColumn { name, available } => {
                assert_eq!(name, "nope");
                assert_eq!(available, vec!["a".to_owned()]);
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn empty_rows_yields_no_data_error() {
        let cols = vec![col("v", "int")];
        let err = derive_chart_data(&ChartConfig::new(ChartKind::Bar), &cols, &[])
            .expect_err("must reject");
        assert_eq!(err, ChartError::NoData);
    }

    #[test]
    fn empty_columns_yields_no_columns_error() {
        let err = derive_chart_data(&ChartConfig::new(ChartKind::Bar), &[], &[])
            .expect_err("must reject");
        assert_eq!(err, ChartError::NoColumns);
    }

    #[test]
    fn line_chart_uses_row_index_when_no_x_specified() {
        let cols = vec![col("v", "int")];
        let rows = vec![row(vec![Value::Int(5)]), row(vec![Value::Int(7)])];
        let data =
            derive_chart_data(&ChartConfig::new(ChartKind::Line), &cols, &rows).expect("derive");
        assert_eq!(data.labels, vec!["1", "2"]);
        assert_eq!(data.values, vec![5.0, 7.0]);
    }

    #[test]
    fn null_y_cells_are_skipped() {
        let cols = vec![col("k", "text"), col("v", "int")];
        let rows = vec![
            row(vec![Value::String("a".into()), Value::Null]),
            row(vec![Value::String("b".into()), Value::Int(2)]),
        ];
        let data =
            derive_chart_data(&ChartConfig::new(ChartKind::Bar), &cols, &rows).expect("derive");
        assert_eq!(data.labels, vec!["b"]);
        assert_eq!(data.values, vec![2.0]);
    }

    #[test]
    fn no_numeric_column_reports_error() {
        let cols = vec![col("a", "text"), col("b", "text")];
        let rows = vec![row(vec![
            Value::String("x".into()),
            Value::String("y".into()),
        ])];
        let err = derive_chart_data(&ChartConfig::new(ChartKind::Bar), &cols, &rows)
            .expect_err("must reject");
        match err {
            ChartError::NoNumericColumn { available } => {
                assert_eq!(available, vec!["a".to_owned(), "b".to_owned()]);
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn config_default_bounds_match_kind() {
        assert_eq!(ChartConfig::new(ChartKind::Bar).bounded_to, 50);
        assert_eq!(ChartConfig::new(ChartKind::Line).bounded_to, 1_000);
        assert_eq!(ChartConfig::new(ChartKind::Sparkline).bounded_to, 1_000);
    }

    #[test]
    fn explicit_title_overrides_default() {
        let cols = vec![col("v", "int")];
        let rows = vec![row(vec![Value::Int(1)])];
        let mut cfg = ChartConfig::new(ChartKind::Bar);
        cfg.title = Some("My Chart".into());
        let data = derive_chart_data(&cfg, &cols, &rows).expect("derive");
        assert_eq!(data.title, "My Chart");
    }
}
