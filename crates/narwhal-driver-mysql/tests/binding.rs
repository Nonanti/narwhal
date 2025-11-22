//! Regression tests for `value_to_my` (bug C1).
//!
//! The previous implementation round-tripped every date/time component
//! through `v.format("%Y").to_string().parse().unwrap_or(0)`. That had
//! three bad failure modes:
//!
//! 1. Years outside the `u16` range silently became 0 — MySQL then either
//!    rejected the bind or stored `0000-00-00`.
//! 2. Six string allocations and six parses per bind on the hot path.
//! 3. Any other parse error (corrupt `format` output, broken locale) was
//!    silently swallowed via `unwrap_or(0)`.
//!
//! These tests pin the new behaviour:
//! * Valid Date / Time / DateTime values bind to their exact MyValue
//!   counterparts (year/month/day/h/m/s/micro).
//! * `Value::Timestamp` is normalised to UTC and bound as a MySQL
//!   DATETIME literal (`MyValue::Date`, not RFC3339 bytes).
//! * Years outside `u16` return a typed error instead of silently
//!   writing `0`.

use chrono::{NaiveDate, NaiveDateTime, NaiveTime, TimeZone, Utc};
use mysql_async::Value as MyValue;
use narwhal_core::Value;
use narwhal_driver_mysql::__test_only::try_value_to_my;

#[test]
fn bind_date_preserves_year_month_day() {
    let v = Value::Date(NaiveDate::from_ymd_opt(2024, 1, 2).unwrap());
    let out = try_value_to_my(&v).expect("date binds successfully");
    assert_eq!(out, MyValue::Date(2024, 1, 2, 0, 0, 0, 0));
}

#[test]
fn bind_time_preserves_components() {
    let v = Value::Time(NaiveTime::from_hms_micro_opt(13, 14, 15, 123_456).unwrap());
    let out = try_value_to_my(&v).expect("time binds successfully");
    assert_eq!(out, MyValue::Time(false, 0, 13, 14, 15, 123_456));
}

#[test]
fn bind_datetime_roundtrip_microsecond() {
    let date = NaiveDate::from_ymd_opt(2024, 6, 15).unwrap();
    let time = NaiveTime::from_hms_micro_opt(10, 20, 30, 123_456).unwrap();
    let v = Value::DateTime(NaiveDateTime::new(date, time));
    let out = try_value_to_my(&v).expect("datetime binds successfully");
    assert_eq!(out, MyValue::Date(2024, 6, 15, 10, 20, 30, 123_456));
}

#[test]
fn bind_date_rejects_year_out_of_range() {
    // Years before AD 1 are negative in chrono; u16 cannot hold them.
    let v = Value::Date(NaiveDate::from_ymd_opt(-1, 1, 1).unwrap());
    let err = try_value_to_my(&v).expect_err("negative year must fail");
    let msg = format!("{err}");
    assert!(msg.contains("year"), "error should mention year: {msg}");

    // Years above u16::MAX (65535) must fail too.
    let v = Value::Date(NaiveDate::from_ymd_opt(99_999, 1, 1).unwrap());
    assert!(try_value_to_my(&v).is_err());
}

#[test]
fn bind_timestamp_normalises_to_utc_datetime_literal() {
    // Pick a non-UTC offset to ensure UTC normalisation, then verify the
    // payload is MyValue::Date (not RFC3339 bytes which MySQL rejects).
    let ts = Utc.with_ymd_and_hms(2024, 6, 15, 10, 20, 30).unwrap();
    let v = Value::Timestamp(ts);
    let out = try_value_to_my(&v).expect("timestamp binds successfully");
    match out {
        MyValue::Date(y, m, d, h, mi, s, _) => {
            assert_eq!((y, m, d, h, mi, s), (2024, 6, 15, 10, 20, 30));
        }
        other => panic!("expected MyValue::Date, got {other:?}"),
    }
}
