//! Regression tests for `Capabilities::streaming` (bug H5).
//!
//! The flag describes whether `Connection::stream` yields rows
//! progressively or materialises the whole result first. Today `MySQL`
//! buffers the entire `QueryResult` before exposing it through a
//! `BufferedRowStream`; the UI must know this so it can warn users
//! against opening open-ended streams over large tables.
//!
//! These tests pin the new field and its builder so future drivers
//! cannot regress the semantics.

use narwhal_core::Capabilities;

#[test]
fn default_streaming_is_false() {
    // A brand-new `Capabilities::default()` must keep `streaming` off so
    // a freshly-added driver that forgets to opt in is treated as
    // buffered.
    let caps = Capabilities::default();
    assert!(
        !caps.streaming,
        "default Capabilities must have streaming = false"
    );
}

#[test]
fn with_streaming_sets_flag_true() {
    let caps = Capabilities::default().with_streaming(true);
    assert!(caps.streaming);
}

#[test]
fn with_streaming_sets_flag_false() {
    let caps = Capabilities::default()
        .with_streaming(true)
        .with_streaming(false);
    assert!(!caps.streaming);
}

#[test]
fn streaming_is_independent_of_other_flags() {
    // Capability flags must not interfere with each other.
    let caps = Capabilities::default()
        .with_transactions(true)
        .with_cancellation(true)
        .with_streaming(true)
        .with_prepared_statements(true);
    assert!(caps.transactions);
    assert!(caps.cancellation);
    assert!(caps.streaming);
    assert!(caps.prepared_statements);
    // Untouched flags stay at default.
    assert!(!caps.savepoints);
    assert!(!caps.rows_affected);
    assert!(!caps.multiple_schemas);
}
