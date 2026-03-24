//! Apache Parquet writer (T1-T4-B).
//!
//! Strategy: walk the first 100 rows to infer an Arrow `DataType` per
//! column, then build typed `ArrayBuilder`s for the full result, push
//! every value through the matching builder, finish the arrays into a
//! single `RecordBatch`, and hand the batch to `ArrowWriter`. The
//! writer is wrapped around a temp file in the destination directory;
//! we rename onto the final path only on success so a mid-stream
//! failure cannot leave a half-written `.parquet` lying around.
//!
//! Memory: the full result is materialised twice (once as `&[Row]`
//! from the caller, once as Arrow arrays inside the builders). The
//! brief explicitly defers a streaming writer to v2.2+ — see
//! `docs/dev/t1-t4-b-parquet-markdown.md`. The 64k-row default row
//! group size means Parquet itself doesn't add memory pressure on
//! top of the builders.

use std::fs::File;
use std::io::BufWriter;
use std::path::Path;
use std::sync::Arc;

use arrow_array::builder::{
    BooleanBuilder, Float64Builder, Int64Builder, StringBuilder, TimestampMicrosecondBuilder,
};
use arrow_array::{ArrayRef, RecordBatch};
use arrow_schema::{DataType, Field, Schema, TimeUnit};
use chrono::{DateTime, Utc};
use narwhal_core::{ColumnHeader, Row, Value};
use parquet::arrow::ArrowWriter;
use parquet::basic::{Compression, ZstdLevel};
use parquet::file::properties::WriterProperties;

use super::error::ExportError;
use super::format::ParquetCompression;

/// Number of leading rows scanned for schema inference. Tuned for the
/// typical "SELECT * FROM `big_table` LIMIT 1000" use case: 100 rows is
/// enough to see whether a column is nullable / mixed-type without
/// adding meaningful latency to small results.
const SCHEMA_SAMPLE: usize = 100;

pub(super) fn write_parquet(
    columns: &[ColumnHeader],
    rows: &[Row],
    path: &Path,
    compression: ParquetCompression,
) -> Result<(), ExportError> {
    // Arrow `RecordBatch::try_new` rejects zero-column schemas. The
    // "no result" path (`:export parquet` on a tab with no header)
    // would otherwise surface as a confusing
    // `RecordBatch.try_new failed` instead of a clear domain error.
    if columns.is_empty() {
        return Err(ExportError::Serialise(
            "parquet export needs at least one column — run a query first".to_owned(),
        ));
    }
    let logical_types: Vec<LogicalType> = columns
        .iter()
        .enumerate()
        .map(|(idx, col)| infer_column_type(idx, col, rows))
        .collect();

    let fields: Vec<Field> = columns
        .iter()
        .zip(logical_types.iter())
        .map(|(col, ty)| Field::new(&col.name, ty.arrow_data_type(), true))
        .collect();
    let schema = Arc::new(Schema::new(fields));

    let mut builders: Vec<ColumnBuilder> = logical_types
        .iter()
        .copied()
        .map(ColumnBuilder::new)
        .collect();

    for row in rows {
        for (idx, value) in row.0.iter().enumerate() {
            // Defensive: a row shorter than the column header could
            // come from a driver mid-rewrite. Treat missing cells as
            // NULL rather than panicking — losing one row's worth of
            // data is much better than dropping the whole export.
            if let Some(builder) = builders.get_mut(idx) {
                builder.append_value(value);
            }
        }
        // And conversely, pad short rows so every builder stays in
        // sync (length-mismatched arrays fail `RecordBatch::try_new`).
        for builder in builders.iter_mut().skip(row.0.len()) {
            builder.append_null();
        }
    }

    let arrays: Vec<ArrayRef> = builders
        .into_iter()
        .map(ColumnBuilder::finish)
        .collect::<Result<Vec<_>, _>>()?;

    let batch = RecordBatch::try_new(Arc::clone(&schema), arrays)
        .map_err(|e| ExportError::Serialise(format!("parquet record batch: {e}")))?;

    let props = WriterProperties::builder()
        .set_compression(compression_codec(compression))
        .build();

    // Atomic write: stage into `*.tmp` next to the destination, fsync
    // through `close()`, then rename. This matches the pattern used
    // by `narwhal-config` for settings.toml so a half-finished export
    // never appears under the user's target path.
    let staging = staging_path(path);
    if let Some(parent) = staging.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }

    let file = File::create(&staging)?;
    let mut writer = ArrowWriter::try_new(BufWriter::new(file), schema, Some(props))
        .map_err(|e| ExportError::Serialise(format!("parquet writer init: {e}")))?;
    if let Err(error) = writer.write(&batch) {
        let _ = std::fs::remove_file(&staging);
        return Err(ExportError::Serialise(format!(
            "parquet write batch: {error}"
        )));
    }
    if let Err(error) = writer.close() {
        let _ = std::fs::remove_file(&staging);
        return Err(ExportError::Serialise(format!(
            "parquet writer close: {error}"
        )));
    }

    if let Err(error) = std::fs::rename(&staging, path) {
        // Atomic-write contract: leave nothing behind on failure.
        let _ = std::fs::remove_file(&staging);
        return Err(ExportError::Io(error));
    }
    Ok(())
}

fn staging_path(target: &Path) -> std::path::PathBuf {
    let mut staging = target.to_path_buf();
    let stem = staging.file_name().map_or_else(
        || "narwhal-export".to_owned(),
        |n| n.to_string_lossy().into_owned(),
    );
    staging.set_file_name(format!(".{stem}.tmp"));
    staging
}

fn compression_codec(compression: ParquetCompression) -> Compression {
    match compression {
        ParquetCompression::Snappy => Compression::SNAPPY,
        // ZstdLevel::default() is level 1 in the parquet crate — fast
        // enough to be a sane default while still beating Snappy on
        // ratio. Power users who want maximum ratio can re-export.
        ParquetCompression::Zstd => Compression::ZSTD(ZstdLevel::default()),
        ParquetCompression::None => Compression::UNCOMPRESSED,
    }
}

/// Logical type used for a single Parquet column.
///
/// We intentionally collapse the rich narwhal `Value` taxonomy onto a
/// small set of physical types: every consumer (Polars, `DuckDB`, Spark)
/// can read these without surprises. Date/Time/Timestamp all map to
/// a single Timestamp(µs, UTC) column because that's the only widely
/// portable temporal type — `DuckDB` and Polars promote bare dates into
/// timestamps anyway.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LogicalType {
    Bool,
    Int64,
    Float64,
    Utf8,
    Timestamp,
}

impl LogicalType {
    fn arrow_data_type(self) -> DataType {
        match self {
            Self::Bool => DataType::Boolean,
            Self::Int64 => DataType::Int64,
            Self::Float64 => DataType::Float64,
            Self::Utf8 => DataType::Utf8,
            Self::Timestamp => DataType::Timestamp(TimeUnit::Microsecond, Some("UTC".into())),
        }
    }

    /// Widen `self` to accommodate a value of category `other`. Used
    /// during inference (`Int64` widens to `Float64` if we later see
    /// a float; mixed columns end up as `Utf8`).
    fn widen(self, other: Self) -> Self {
        use LogicalType::{Bool, Float64, Int64, Timestamp, Utf8};
        match (self, other) {
            (a, b) if a == b => a,
            (Int64, Float64) | (Float64, Int64) => Float64,
            // Bool + numeric → numeric (rare; happens with SQLite
            // `INTEGER` columns that happen to only hold 0/1 in the
            // sample but a non-bool elsewhere).
            (Bool, Int64) | (Int64, Bool) => Int64,
            (Bool, Float64) | (Float64, Bool) => Float64,
            // Any mismatch we can't reconcile falls back to Utf8 — the
            // brief calls this out explicitly under "Tricky bits ›
            // Mixed-type columns".
            (Timestamp, _) | (_, Timestamp) => Utf8,
            _ => Utf8,
        }
    }
}

fn infer_column_type(idx: usize, header: &ColumnHeader, rows: &[Row]) -> LogicalType {
    // Start from the header type so an all-NULL sample doesn't strand
    // the column on a default that disagrees with the schema later.
    let mut inferred: Option<LogicalType> = type_hint_from_header(&header.data_type);
    for row in rows.iter().take(SCHEMA_SAMPLE) {
        let Some(value) = row.0.get(idx) else {
            continue;
        };
        let Some(observed) = type_from_value(value) else {
            continue;
        };
        inferred = Some(match inferred {
            Some(existing) => existing.widen(observed),
            None => observed,
        });
        if matches!(inferred, Some(LogicalType::Utf8)) {
            // No point widening further once we've degraded to Utf8.
            break;
        }
    }
    inferred.unwrap_or(LogicalType::Utf8)
}

fn type_hint_from_header(data_type: &str) -> Option<LogicalType> {
    let lower = data_type.to_ascii_lowercase();
    if lower.contains("bool") {
        Some(LogicalType::Bool)
    } else if lower.contains("int") || lower.contains("serial") {
        Some(LogicalType::Int64)
    } else if ["real", "float", "double", "decimal", "numeric", "money"]
        .iter()
        .any(|hint| lower.contains(hint))
    {
        // Decimal degrades to Float64 here — the brief notes precision
        // can be unknown; we'd lose the exact representation either
        // way (Arrow's Decimal128 needs (precision, scale) up-front).
        Some(LogicalType::Float64)
    } else if lower.contains("timestamp") || lower.contains("date") || lower.contains("time") {
        Some(LogicalType::Timestamp)
    } else {
        None
    }
}

const fn type_from_value(value: &Value) -> Option<LogicalType> {
    match value {
        Value::Null => None,
        Value::Bool(_) => Some(LogicalType::Bool),
        Value::Int(_) => Some(LogicalType::Int64),
        Value::Float(_) => Some(LogicalType::Float64),
        // `Time` is a wall-clock time without a date — there is no
        // sensible mapping to an absolute Timestamp(µs, UTC) and no
        // portable Arrow type for it (Time32/Time64 confuses every
        // Polars/DuckDB consumer we tried). Keep it as a string so the
        // export round-trips losslessly via display.
        Value::Date(_) | Value::DateTime(_) | Value::Timestamp(_) => Some(LogicalType::Timestamp),
        // Everything stringy (including UUID, JSON, Bytes, Unknown,
        // Time) collapses to Utf8. UUID/JSON could in principle be
        // their own logical types but every Parquet reader handles
        // strings, and round-tripping JSON via string keeps the
        // schema simple.
        _ => Some(LogicalType::Utf8),
    }
}

/// Wraps one of the typed Arrow builders. We dispatch through a small
/// enum rather than `Box<dyn ArrayBuilder>` because the builder traits
/// in arrow-array are not object-safe across `append_value`.
enum ColumnBuilder {
    Bool(BooleanBuilder),
    Int64(Int64Builder),
    Float64(Float64Builder),
    Utf8(StringBuilder),
    Timestamp(TimestampMicrosecondBuilder),
}

impl ColumnBuilder {
    fn new(logical: LogicalType) -> Self {
        match logical {
            LogicalType::Bool => Self::Bool(BooleanBuilder::new()),
            LogicalType::Int64 => Self::Int64(Int64Builder::new()),
            LogicalType::Float64 => Self::Float64(Float64Builder::new()),
            LogicalType::Utf8 => Self::Utf8(StringBuilder::new()),
            LogicalType::Timestamp => {
                Self::Timestamp(TimestampMicrosecondBuilder::new().with_timezone(Arc::from("UTC")))
            }
        }
    }

    fn append_null(&mut self) {
        match self {
            Self::Bool(b) => b.append_null(),
            Self::Int64(b) => b.append_null(),
            Self::Float64(b) => b.append_null(),
            Self::Utf8(b) => b.append_null(),
            Self::Timestamp(b) => b.append_null(),
        }
    }

    fn append_value(&mut self, value: &Value) {
        // Null short-circuits before the typed dispatch so every
        // builder variant reaches `append_null` regardless of its
        // inferred schema. Doing this inside `match (self, value)`
        // moves `self` out of the borrow and trips E0382.
        if matches!(value, Value::Null) {
            self.append_null();
            return;
        }
        match (self, value) {
            (Self::Bool(b), Value::Bool(v)) => b.append_value(*v),
            (Self::Bool(b), Value::Int(n)) => b.append_value(*n != 0),
            (Self::Int64(b), Value::Int(n)) => b.append_value(*n),
            (Self::Int64(b), Value::Bool(v)) => b.append_value(i64::from(*v)),
            (Self::Float64(b), Value::Float(n)) => b.append_value(*n),
            (Self::Float64(b), Value::Int(n)) => {
                // i64 → f64 loses precision for |n| > 2^53. Accept the
                // loss: the only narwhal value flowing through a
                // widened Float64 column is one we've already decided
                // is numerically mixed; users who need exact ints
                // shouldn't be exporting them through a column we
                // widened.
                #[allow(clippy::cast_precision_loss)]
                b.append_value(*n as f64);
            }
            (Self::Float64(b), Value::Bool(v)) => b.append_value(f64::from(i32::from(*v))),
            (Self::Utf8(b), other) => b.append_value(other.render()),
            (Self::Timestamp(b), Value::Timestamp(ts)) => {
                b.append_value(ts.timestamp_micros());
            }
            (Self::Timestamp(b), Value::DateTime(dt)) => {
                let utc: DateTime<Utc> = DateTime::from_naive_utc_and_offset(*dt, Utc);
                b.append_value(utc.timestamp_micros());
            }
            (Self::Timestamp(b), Value::Date(d)) => {
                if let Some(dt) = d.and_hms_opt(0, 0, 0) {
                    let utc: DateTime<Utc> = DateTime::from_naive_utc_and_offset(dt, Utc);
                    b.append_value(utc.timestamp_micros());
                } else {
                    b.append_null();
                }
            }
            // Mismatched value vs. inferred type — should be rare
            // (would require a value beyond row SCHEMA_SAMPLE that
            // doesn't fit). Render to string on Utf8 columns is
            // already handled above; for typed columns we drop the
            // value as null rather than crash.
            (typed, _) => typed.append_null(),
        }
    }

    fn finish(mut self) -> Result<ArrayRef, ExportError> {
        let array: ArrayRef = match &mut self {
            Self::Bool(b) => Arc::new(b.finish()),
            Self::Int64(b) => Arc::new(b.finish()),
            Self::Float64(b) => Arc::new(b.finish()),
            Self::Utf8(b) => Arc::new(b.finish()),
            Self::Timestamp(b) => Arc::new(b.finish()),
        };
        Ok(array)
    }
}
