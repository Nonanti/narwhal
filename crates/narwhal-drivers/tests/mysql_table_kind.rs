//! Regression tests for `map_table_kind` and the `describe_table` use
//! site (bug L30).
//!
//! `describe_table` hard-coded '`TableKind::Table`' regardless of whether
//! the object was a view, system view, or materialised view. The
//! sidebar therefore showed every view with the table icon and the
//! "View Schema" command refused to identify it. `list_tables` already
//! reported the kind correctly, so the two code paths disagreed.
//!
//! The fix is twofold: extract a '`map_table_kind`' helper (unit tested
//! here) and have '`describe_table`' query '`information_schema.tables`'
//! to populate the field. Only the helper can be tested without a live
//! `MySQL` instance; the `describe_table` call site is exercised via
//! integration tests.

use narwhal_core::TableKind;
use narwhal_drivers::mysql::__test_only::map_table_kind;

#[test]
fn view_maps_to_view_kind() {
    assert_eq!(map_table_kind(Some("VIEW")), TableKind::View);
}

#[test]
fn base_table_maps_to_table_kind() {
    assert_eq!(map_table_kind(Some("BASE TABLE")), TableKind::Table);
}

#[test]
fn system_view_maps_to_system_table_kind() {
    assert_eq!(map_table_kind(Some("SYSTEM VIEW")), TableKind::SystemTable);
}

#[test]
fn system_table_maps_to_system_table_kind() {
    assert_eq!(map_table_kind(Some("SYSTEM TABLE")), TableKind::SystemTable);
}

#[test]
fn unknown_kind_falls_back_to_table() {
    assert_eq!(map_table_kind(Some("SEQUENCE")), TableKind::Table);
}

#[test]
fn missing_kind_falls_back_to_table() {
    assert_eq!(map_table_kind(None), TableKind::Table);
}

#[test]
fn empty_string_falls_back_to_table() {
    assert_eq!(map_table_kind(Some("")), TableKind::Table);
}
