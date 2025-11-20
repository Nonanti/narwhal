//! Regression tests for `has_returning_clause` (bug C3).
//!
//! The helper used to do `&sql[i..i + 9]` byte-slice comparisons; when the
//! slice boundary fell in the middle of a multibyte character the panic
//! happened inside `spawn_blocking` and the user only saw a closed channel.

// Re-export the private helper for testing via the public surface.
// We exercise `prepare_modify_statement` indirectly through a dedicated
// helper exposed only under cfg(test) in the driver crate.
//
// As the helper itself is private, we instead trigger the same call path
// through a small public reproducer: the driver tracker uses this helper
// for every statement, but it's pure CPU. To keep this test hermetic we
// invoke the in-crate test-only helper through a small wrapper.

use narwhal_driver_duckdb::__test_only::has_returning_clause;

#[test]
fn returning_detection_does_not_panic_on_multibyte() {
    // Pre-fix panic: `r` at i=0, 7 ASCII chars after, then `ü` (2 bytes
    // starting at byte 8). `sql[0..9]` ends at byte 9 = the second byte of
    // `ü` — not a char boundary, so the original `&str` slice would panic.
    let sql = "rabcdefgümore";
    assert!(!has_returning_clause(sql));

    // Mixed case path: `R` followed by 7 ASCII chars then a multibyte.
    let sql_upper = "R1234567çtrailing";
    assert!(!has_returning_clause(sql_upper));

    // The classic bug.md example: comment with Turkish chars then a query.
    let sql_comment = "-- rüya x\nSELECT 1";
    assert!(!has_returning_clause(sql_comment));

    // Emoji (4-byte) inside a string literal followed by other text.
    let sql_emoji = "SELECT '🦀 narwhal' FROM t";
    assert!(!has_returning_clause(sql_emoji));
}

#[test]
fn returning_detection_handles_word_boundary() {
    // `customer_returning` must not trigger a false positive.
    assert!(!has_returning_clause(
        "INSERT INTO customer_returning VALUES (1)"
    ));
    assert!(!has_returning_clause(
        "SELECT returningish FROM t"
    ));
    assert!(!has_returning_clause(
        "SELECT * FROM areturning"
    ));
}

#[test]
fn returning_detection_finds_real_returning() {
    assert!(has_returning_clause(
        "INSERT INTO t VALUES (1) RETURNING id"
    ));
    assert!(has_returning_clause(
        "DELETE FROM t WHERE id = 1 returning *"
    ));
    // Inside a string literal — must not match.
    assert!(!has_returning_clause(
        "INSERT INTO t VALUES ('RETURNING')"
    ));
    assert!(!has_returning_clause(
        "INSERT INTO t VALUES (\"RETURNING\")"
    ));
}
