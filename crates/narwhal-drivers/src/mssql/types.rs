//! Conversion layer between [`narwhal_core::Value`] and `tiberius`.
//!
//! Tiberius models a TDS column value as [`ColumnData`]. The mapping
//! below is loss-aware: every native MSSQL scalar reaches one of our
//! [`Value`] variants without an intermediate textual round-trip, with
//! the deliberate exception of `numeric`/`decimal` which we render as
//! their decimal string representation to avoid pulling `rust_decimal`
//! into the workspace just for the driver. Loss is documented per
//! variant.
//!
//! The `Param` newtype provides a `ToSql` bridge so callers can hand
//! [`Value`] slices to `tiberius::Client::query` / `execute` without
//! per-call boilerplate.

use std::borrow::Cow;

use chrono::{DateTime, NaiveDate, NaiveDateTime, NaiveTime, Utc};
use narwhal_core::{ColumnHeader, Error, Result, Value};
use tiberius::numeric::Numeric;
use tiberius::xml::XmlData;
use tiberius::{Column, ColumnData, ColumnType, IntoSql, Row, ToSql};

/// Bind newtype: lifts [`Value`] into the [`ToSql`] interface tiberius
/// expects for parameter binding. The conversion is by-reference so the
/// caller's `Vec<Value>` can be re-used across retries.
pub(crate) struct Param<'a>(pub &'a Value);

impl ToSql for Param<'_> {
    fn to_sql(&self) -> ColumnData<'_> {
        match self.0 {
            // Tiberius represents NULL as the `None` arm of whichever
            // ColumnData variant matches the parameter slot's declared
            // type. m1: a typeless NULL is bound as `String(None)`
            // (i.e. `nvarchar(max) NULL`) rather than `I32(None)`
            // because SQL Server coerces `nvarchar` to every other
            // scalar more permissively than it does an int — a typed
            // sproc with an `nvarchar` parameter would reject an
            // `int NULL` binding outright.
            Value::Null => ColumnData::String(None),
            Value::Bool(v) => ColumnData::Bit(Some(*v)),
            Value::Int(v) => ColumnData::I64(Some(*v)),
            Value::Float(v) => ColumnData::F64(Some(*v)),
            Value::String(v) => ColumnData::String(Some(Cow::Borrowed(v.as_str()))),
            Value::Bytes(v) => ColumnData::Binary(Some(Cow::Borrowed(v.as_slice()))),
            // Date/Time/DateTime bind through tiberius' `chrono`
            // `IntoSql` impls so the wire shape matches what the server
            // negotiated (TDS 7.3 → `date`/`time`/`datetime2`).
            Value::Date(v) => v.into_sql(),
            Value::Time(v) => v.into_sql(),
            Value::DateTime(v) => v.into_sql(),
            // `DateTime<Utc>` lands as `datetimeoffset` so the offset
            // round-trips through the wire — using `NaiveDateTime`
            // would silently drop the UTC anchor.
            Value::Timestamp(v) => v.into_sql(),
            Value::Uuid(v) => ColumnData::Guid(Some(*v)),
            // SQL Server has no native JSON type before 2025-preview;
            // encode as `nvarchar(max)` text which matches the
            // canonical workaround.
            Value::Json(v) => ColumnData::String(Some(Cow::Owned(v.to_string()))),
            Value::Unknown(v) => ColumnData::String(Some(Cow::Borrowed(v.as_str()))),
            // Forward-compat: bind future Value variants as their Debug
            // text. Round-trips lossily, but does not panic the driver.
            other => ColumnData::String(Some(Cow::Owned(format!("{other:?}")))),
        }
    }
}

/// Extract column `idx` of `row` as a [`Value`]. The wire type
/// (`column.column_type()`) drives the dispatch; unknown types fall
/// back to a debug rendering via [`Value::Unknown`] so the user can
/// still inspect the row.
///
/// `try_get` is used everywhere so a single conversion failure
/// surfaces as `Error::Query` instead of panicking — bug-class
/// parity with the postgres driver's `column_to_value`.
pub(crate) fn column_to_value(row: &Row, idx: usize, ty: ColumnType) -> Result<Value> {
    // Tiberius' `FromSql` impls already cover `Option<T>` for every
    // Rust scalar in the table below, so `try_get::<T, _>(idx)`
    // returns `Ok(None)` for SQL NULL and `Ok(Some(v))` otherwise.
    macro_rules! get {
        ($t:ty, $map:expr) => {{
            match row.try_get::<$t, _>(idx) {
                Ok(Some(v)) => Ok($map(v)),
                Ok(None) => Ok(Value::Null),
                Err(error) => Err(Error::Query(error.to_string())),
            }
        }};
    }

    match ty {
        ColumnType::Bit | ColumnType::Bitn => get!(bool, Value::Bool),
        ColumnType::Int1 => get!(u8, |v| Value::Int(i64::from(v))),
        ColumnType::Int2 => get!(i16, |v| Value::Int(i64::from(v))),
        ColumnType::Int4 => get!(i32, |v| Value::Int(i64::from(v))),
        ColumnType::Int8 => get!(i64, Value::Int),
        // `Intn` is the "variable-width integer" slot used by parameter
        // markers — at the row level it always decodes to one of the
        // fixed widths above, but cells from `SELECT 1` come back as
        // Intn. Probe the wider widths in order.
        ColumnType::Intn => match row.try_get::<i64, _>(idx) {
            Ok(Some(v)) => Ok(Value::Int(v)),
            Ok(None) => Ok(Value::Null),
            Err(_) => match row.try_get::<i32, _>(idx) {
                Ok(Some(v)) => Ok(Value::Int(i64::from(v))),
                Ok(None) => Ok(Value::Null),
                Err(_) => match row.try_get::<i16, _>(idx) {
                    Ok(Some(v)) => Ok(Value::Int(i64::from(v))),
                    Ok(None) => Ok(Value::Null),
                    Err(_) => get!(u8, |v: u8| Value::Int(i64::from(v))),
                },
            },
        },
        ColumnType::Float4 => get!(f32, |v| Value::Float(f64::from(v))),
        ColumnType::Float8 => get!(f64, Value::Float),
        ColumnType::Floatn => match row.try_get::<f64, _>(idx) {
            Ok(Some(v)) => Ok(Value::Float(v)),
            Ok(None) => Ok(Value::Null),
            Err(_) => get!(f32, |v: f32| Value::Float(f64::from(v))),
        },
        ColumnType::Money | ColumnType::Money4 => {
            // money/smallmoney decode through Numeric.
            get!(Numeric, |v: Numeric| Value::String(v.to_string()))
        }
        ColumnType::Decimaln | ColumnType::Numericn => {
            get!(Numeric, |v: Numeric| Value::String(v.to_string()))
        }
        ColumnType::Guid => get!(uuid::Uuid, Value::Uuid),
        ColumnType::BigChar
        | ColumnType::BigVarChar
        | ColumnType::Text
        | ColumnType::NChar
        | ColumnType::NVarchar
        | ColumnType::NText => get!(&str, |s: &str| Value::String(s.to_owned())),
        ColumnType::BigBinary | ColumnType::BigVarBin | ColumnType::Image => {
            get!(&[u8], |b: &[u8]| Value::Bytes(b.to_vec()))
        }
        ColumnType::Datetime | ColumnType::Datetime4 | ColumnType::Datetimen => {
            get!(NaiveDateTime, Value::DateTime)
        }
        ColumnType::Datetime2 => get!(NaiveDateTime, Value::DateTime),
        ColumnType::DatetimeOffsetn => get!(DateTime<Utc>, Value::Timestamp),
        ColumnType::Daten => get!(NaiveDate, Value::Date),
        ColumnType::Timen => get!(NaiveTime, Value::Time),
        ColumnType::Xml => get!(&XmlData, |x: &XmlData| Value::String(x.to_string())),
        // SQL_VARIANT, UDT, and anything else we don't model: try the
        // generic string path so simple textual values come through,
        // then fall back to `Unknown` with the type name.
        _ => match row.try_get::<&str, _>(idx) {
            Ok(Some(s)) => Ok(Value::Unknown(s.to_owned())),
            Ok(None) => Ok(Value::Null),
            Err(_) => Ok(Value::Unknown(format!("<{}>", column_type_name(ty)))),
        },
    }
}

/// Map a tiberius [`Column`] to our [`ColumnHeader`]. The native type
/// rendering is the lower-cased variant of [`ColumnType`] so the
/// TUI / MCP layer can match on it the same way it does for the other
/// drivers.
pub(crate) fn column_header(column: &Column) -> ColumnHeader {
    ColumnHeader {
        name: column.name().to_owned(),
        data_type: column_type_name(column.column_type()),
    }
}

/// Lower-case, hyphen-free rendering of a [`ColumnType`]. Mirrors the
/// shape postgres / mysql produce, so cross-driver UI consumers don't
/// need a per-engine lookup table.
//
// The wildcard arm is dead code today (tiberius' `ColumnType` is fully
// covered) but kept as a forward-compatibility hatch so a future
// tiberius release that adds a variant doesn't break this driver's
// build.
#[allow(unreachable_patterns)]
pub(crate) fn column_type_name(ty: ColumnType) -> String {
    let name = match ty {
        ColumnType::Null => "null",
        ColumnType::Bit | ColumnType::Bitn => "bit",
        ColumnType::Int1 => "tinyint",
        ColumnType::Int2 => "smallint",
        ColumnType::Int4 => "int",
        ColumnType::Int8 => "bigint",
        ColumnType::Intn => "int",
        ColumnType::Float4 => "real",
        ColumnType::Float8 => "float",
        ColumnType::Floatn => "float",
        ColumnType::Money | ColumnType::Money4 => "money",
        ColumnType::Datetime | ColumnType::Datetime4 | ColumnType::Datetimen => "datetime",
        ColumnType::Datetime2 => "datetime2",
        ColumnType::DatetimeOffsetn => "datetimeoffset",
        ColumnType::Daten => "date",
        ColumnType::Timen => "time",
        ColumnType::Guid => "uniqueidentifier",
        ColumnType::Decimaln | ColumnType::Numericn => "decimal",
        ColumnType::BigChar | ColumnType::BigVarChar | ColumnType::Text => "varchar",
        ColumnType::NChar | ColumnType::NVarchar | ColumnType::NText => "nvarchar",
        ColumnType::BigBinary | ColumnType::BigVarBin | ColumnType::Image => "varbinary",
        ColumnType::Udt => "udt",
        ColumnType::Xml => "xml",
        ColumnType::SSVariant => "sql_variant",
        _ => "unknown",
    };
    name.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn column_type_name_covers_common_types() {
        assert_eq!(column_type_name(ColumnType::Int4), "int");
        assert_eq!(column_type_name(ColumnType::NVarchar), "nvarchar");
        assert_eq!(column_type_name(ColumnType::Guid), "uniqueidentifier");
        assert_eq!(column_type_name(ColumnType::Datetime2), "datetime2");
        assert_eq!(column_type_name(ColumnType::Decimaln), "decimal");
        assert_eq!(column_type_name(ColumnType::BigVarBin), "varbinary");
    }

    #[test]
    fn param_binds_basic_values() {
        let v = Value::String("hello".into());
        match Param(&v).to_sql() {
            ColumnData::String(Some(c)) => assert_eq!(c.as_ref(), "hello"),
            other => panic!("unexpected {other:?}"),
        }

        let v = Value::Int(42);
        assert!(matches!(Param(&v).to_sql(), ColumnData::I64(Some(42))));

        let v = Value::Bool(true);
        assert!(matches!(Param(&v).to_sql(), ColumnData::Bit(Some(true))));

        // m1: typeless NULL binds as nvarchar (most permissive).
        let v = Value::Null;
        assert!(matches!(Param(&v).to_sql(), ColumnData::String(None)));

        let v = Value::Bytes(vec![1, 2, 3]);
        match Param(&v).to_sql() {
            ColumnData::Binary(Some(b)) => assert_eq!(&*b, &[1, 2, 3][..]),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn param_uuid_binds_as_guid() {
        let uuid = uuid::Uuid::new_v4();
        let v = Value::Uuid(uuid);
        match Param(&v).to_sql() {
            ColumnData::Guid(Some(g)) => assert_eq!(g, uuid),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn param_json_renders_as_text() {
        // `Value::Json(serde_json::Value)` rides on narwhal-core's
        // public API; we parse a literal string rather than pull the
        // `json!` macro crate into this driver's dep tree.
        let value = Value::Json("42".parse().expect("json number parses"));
        match Param(&value).to_sql() {
            ColumnData::String(Some(c)) => assert_eq!(c.as_ref(), "42"),
            other => panic!("unexpected {other:?}"),
        }
    }
}
