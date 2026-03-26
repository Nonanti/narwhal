//! Conversion layer between [`narwhal_core::Value`] and `tokio-postgres`.

use bytes::BytesMut;
use chrono::{DateTime, NaiveDate, NaiveDateTime, NaiveTime, Utc};
use narwhal_core::{Error, Result, Value};
use tokio_postgres::Row;
use tokio_postgres::types::{IsNull, ToSql, Type, to_sql_checked};

/// Newtype wrapping [`Value`] to provide a [`ToSql`] implementation.
///
/// `ToSql` is required when binding parameters through `tokio-postgres`.
/// `Value` is defined in `narwhal-core` which has no `PostgreSQL` dependency,
/// so the bridge lives here.
pub(crate) struct Param<'a>(pub &'a Value);

impl std::fmt::Debug for Param<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(self.0, f)
    }
}

impl ToSql for Param<'_> {
    fn to_sql(
        &self,
        ty: &Type,
        out: &mut BytesMut,
    ) -> std::result::Result<IsNull, Box<dyn std::error::Error + Sync + Send>> {
        match self.0 {
            Value::Null => Ok(IsNull::Yes),
            Value::Bool(v) => v.to_sql(ty, out),
            Value::Int(v) => match *ty {
                // L8: refuse the bind instead of letting the lossy `as`
                // cast silently emit the truncated value.
                Type::INT2 => i16::try_from(*v)
                    .map_err(|_| -> Box<dyn std::error::Error + Sync + Send> {
                        format!("value {v} out of range for INT2").into()
                    })?
                    .to_sql(ty, out),
                Type::INT4 => i32::try_from(*v)
                    .map_err(|_| -> Box<dyn std::error::Error + Sync + Send> {
                        format!("value {v} out of range for INT4").into()
                    })?
                    .to_sql(ty, out),
                Type::OID => u32::try_from(*v)
                    .map_err(|_| -> Box<dyn std::error::Error + Sync + Send> {
                        format!("value {v} out of range for OID").into()
                    })?
                    .to_sql(ty, out),
                _ => v.to_sql(ty, out),
            },
            Value::Float(v) => match *ty {
                Type::FLOAT4 => (*v as f32).to_sql(ty, out),
                _ => v.to_sql(ty, out),
            },
            Value::String(v) => v.to_sql(ty, out),
            Value::Bytes(v) => v.to_sql(ty, out),
            Value::Date(v) => v.to_sql(ty, out),
            Value::Time(v) => v.to_sql(ty, out),
            Value::DateTime(v) => v.to_sql(ty, out),
            Value::Timestamp(v) => v.to_sql(ty, out),
            Value::Uuid(v) => v.to_sql(ty, out),
            Value::Json(v) => v.to_sql(ty, out),
            Value::Unknown(v) => v.to_sql(ty, out),
            // Forward-compatible: bind future Value variants as their Debug repr.
            other => format!("{other:?}").to_sql(ty, out),
        }
    }

    fn accepts(_ty: &Type) -> bool {
        // Permissive: the underlying conversion errors out at runtime if the
        // bound value is incompatible with the parameter type.
        true
    }

    to_sql_checked!();
}

/// Convert a single column of a `tokio-postgres` row into a [`Value`].
pub(crate) fn column_to_value(row: &Row, idx: usize, ty: &Type) -> Result<Value> {
    macro_rules! get {
        ($t:ty, $map:expr) => {{
            match row.try_get::<_, Option<$t>>(idx) {
                Ok(Some(v)) => Ok($map(v)),
                Ok(None) => Ok(Value::Null),
                Err(error) => Err(Error::query_with("tokio-postgres column read", error)),
            }
        }};
    }

    match *ty {
        Type::BOOL => get!(bool, Value::Bool),
        Type::INT2 => get!(i16, |v| Value::Int(i64::from(v))),
        Type::INT4 => get!(i32, |v| Value::Int(i64::from(v))),
        Type::INT8 => get!(i64, Value::Int),
        Type::OID => get!(u32, |v| Value::Int(i64::from(v))),
        Type::FLOAT4 => get!(f32, |v| Value::Float(f64::from(v))),
        Type::FLOAT8 => get!(f64, Value::Float),
        Type::TEXT | Type::VARCHAR | Type::BPCHAR | Type::NAME | Type::CHAR_ARRAY => {
            get!(String, Value::String)
        }
        Type::BYTEA => get!(Vec<u8>, Value::Bytes),
        Type::DATE => get!(NaiveDate, Value::Date),
        Type::TIME => get!(NaiveTime, Value::Time),
        Type::TIMESTAMP => get!(NaiveDateTime, Value::DateTime),
        Type::TIMESTAMPTZ => get!(DateTime<Utc>, Value::Timestamp),
        Type::UUID => get!(uuid::Uuid, Value::Uuid),
        Type::JSON | Type::JSONB => get!(serde_json::Value, Value::Json),
        _ => {
            // Fallback: try to render the value as text. Unknown OIDs are
            // surfaced as [`Value::Unknown`] rather than producing an error
            // so the user can still inspect the row.
            match row.try_get::<_, Option<String>>(idx) {
                Ok(Some(text)) => Ok(Value::Unknown(text)),
                Ok(None) => Ok(Value::Null),
                Err(_) => Ok(Value::Unknown(format!("<{}>", ty.name()))),
            }
        }
    }
}

#[allow(dead_code)]
fn _assert_traits() {
    const fn ensure_sync<T: Sync>(_: &T) {}
    let value = Value::Null;
    let param = Param(&value);
    ensure_sync(&param);
}
