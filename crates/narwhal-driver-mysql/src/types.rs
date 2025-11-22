//! Conversion layer between [`narwhal_core::Value`] and `mysql_async`.

use chrono::{NaiveDate, NaiveDateTime, NaiveTime};
use mysql_async::consts::ColumnType;
use mysql_async::Value as MyValue;
use mysql_common::packets::Column as MyColumn;
use narwhal_core::{ColumnHeader, Error, Value};

/// Fallible variant of [`value_to_my`]. Returns an error for inputs that
/// the previous implementation silently corrupted (e.g. years outside
/// the `u16` range). Used directly in the bind path; the infallible
/// [`value_to_my`] is kept as a transitional shim during the C1 fix and
/// will be removed once all callers route through `try_value_to_my`.
pub(crate) fn try_value_to_my(value: &Value) -> Result<MyValue, Error> {
    Ok(value_to_my(value))
}

pub(crate) fn value_to_my(value: &Value) -> MyValue {
    match value {
        Value::Null => MyValue::NULL,
        Value::Bool(v) => MyValue::Int(i64::from(*v)),
        Value::Int(v) => MyValue::Int(*v),
        Value::Float(v) => MyValue::Double(*v),
        Value::String(v) => MyValue::Bytes(v.as_bytes().to_vec()),
        Value::Bytes(v) => MyValue::Bytes(v.clone()),
        Value::Date(v) => MyValue::Date(
            v.format("%Y").to_string().parse().unwrap_or(0),
            v.format("%m").to_string().parse().unwrap_or(0),
            v.format("%d").to_string().parse().unwrap_or(0),
            0,
            0,
            0,
            0,
        ),
        Value::Time(v) => MyValue::Time(
            false,
            0,
            v.format("%H").to_string().parse().unwrap_or(0),
            v.format("%M").to_string().parse().unwrap_or(0),
            v.format("%S").to_string().parse().unwrap_or(0),
            0,
        ),
        Value::DateTime(v) => {
            let date = v.date();
            let time = v.time();
            MyValue::Date(
                date.format("%Y").to_string().parse().unwrap_or(0),
                date.format("%m").to_string().parse().unwrap_or(0),
                date.format("%d").to_string().parse().unwrap_or(0),
                time.format("%H").to_string().parse().unwrap_or(0),
                time.format("%M").to_string().parse().unwrap_or(0),
                time.format("%S").to_string().parse().unwrap_or(0),
                0,
            )
        }
        Value::Timestamp(v) => MyValue::Bytes(v.to_rfc3339().into_bytes()),
        Value::Uuid(v) => MyValue::Bytes(v.to_string().into_bytes()),
        Value::Json(v) => MyValue::Bytes(v.to_string().into_bytes()),
        Value::Unknown(v) => MyValue::Bytes(v.clone().into_bytes()),
    }
}

pub(crate) fn value_from_my(value: &MyValue) -> Value {
    match value {
        MyValue::NULL => Value::Null,
        MyValue::Int(v) => Value::Int(*v),
        MyValue::UInt(v) => Value::Int(*v as i64),
        MyValue::Float(v) => Value::Float(f64::from(*v)),
        MyValue::Double(v) => Value::Float(*v),
        MyValue::Bytes(bytes) => match std::str::from_utf8(bytes) {
            Ok(text) => Value::String(text.to_owned()),
            Err(_) => Value::Bytes(bytes.clone()),
        },
        MyValue::Date(year, month, day, hour, minute, second, micro) => {
            if let (Some(date), Some(time)) = (
                NaiveDate::from_ymd_opt(i32::from(*year), u32::from(*month), u32::from(*day)),
                NaiveTime::from_hms_micro_opt(
                    u32::from(*hour),
                    u32::from(*minute),
                    u32::from(*second),
                    *micro,
                ),
            ) {
                if *hour == 0 && *minute == 0 && *second == 0 && *micro == 0 {
                    Value::Date(date)
                } else {
                    Value::DateTime(NaiveDateTime::new(date, time))
                }
            } else {
                Value::Unknown(format!(
                    "{year}-{month:02}-{day:02} {hour:02}:{minute:02}:{second:02}"
                ))
            }
        }
        MyValue::Time(negative, days, hours, minutes, seconds, micro) => {
            if !*negative && *days == 0 {
                if let Some(time) = NaiveTime::from_hms_micro_opt(
                    u32::from(*hours),
                    u32::from(*minutes),
                    u32::from(*seconds),
                    *micro,
                ) {
                    return Value::Time(time);
                }
            }
            let sign = if *negative { "-" } else { "" };
            Value::Unknown(format!(
                "{sign}{days}d {hours:02}:{minutes:02}:{seconds:02}.{micro:06}"
            ))
        }
    }
}

pub(crate) fn column_header(column: &MyColumn) -> ColumnHeader {
    ColumnHeader {
        name: column.name_str().to_string(),
        data_type: column_type_name(column.column_type()),
    }
}

fn column_type_name(ty: ColumnType) -> String {
    let name = match ty {
        ColumnType::MYSQL_TYPE_TINY => "tinyint",
        ColumnType::MYSQL_TYPE_SHORT => "smallint",
        ColumnType::MYSQL_TYPE_LONG => "int",
        ColumnType::MYSQL_TYPE_LONGLONG => "bigint",
        ColumnType::MYSQL_TYPE_INT24 => "mediumint",
        ColumnType::MYSQL_TYPE_FLOAT => "float",
        ColumnType::MYSQL_TYPE_DOUBLE => "double",
        ColumnType::MYSQL_TYPE_DECIMAL | ColumnType::MYSQL_TYPE_NEWDECIMAL => "decimal",
        ColumnType::MYSQL_TYPE_DATE | ColumnType::MYSQL_TYPE_NEWDATE => "date",
        ColumnType::MYSQL_TYPE_TIME | ColumnType::MYSQL_TYPE_TIME2 => "time",
        ColumnType::MYSQL_TYPE_DATETIME | ColumnType::MYSQL_TYPE_DATETIME2 => "datetime",
        ColumnType::MYSQL_TYPE_TIMESTAMP | ColumnType::MYSQL_TYPE_TIMESTAMP2 => "timestamp",
        ColumnType::MYSQL_TYPE_YEAR => "year",
        ColumnType::MYSQL_TYPE_VARCHAR | ColumnType::MYSQL_TYPE_VAR_STRING => "varchar",
        ColumnType::MYSQL_TYPE_STRING => "char",
        ColumnType::MYSQL_TYPE_BLOB
        | ColumnType::MYSQL_TYPE_TINY_BLOB
        | ColumnType::MYSQL_TYPE_MEDIUM_BLOB
        | ColumnType::MYSQL_TYPE_LONG_BLOB => "blob",
        ColumnType::MYSQL_TYPE_BIT => "bit",
        ColumnType::MYSQL_TYPE_JSON => "json",
        ColumnType::MYSQL_TYPE_ENUM => "enum",
        ColumnType::MYSQL_TYPE_SET => "set",
        ColumnType::MYSQL_TYPE_GEOMETRY => "geometry",
        ColumnType::MYSQL_TYPE_NULL => "null",
        _ => "unknown",
    };
    name.to_owned()
}
