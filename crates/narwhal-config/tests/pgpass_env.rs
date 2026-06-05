//! Integration tests for env-var resolution in `narwhal_config::pgpass`.
//!
//! Lives outside the `lib.rs` tree because the lib forbids `unsafe_code`
//! and Rust 2024 marks `std::env::set_var`/`remove_var` as `unsafe`
//! (they race with libc `getenv` in other threads). All env mutation in
//! this file is single-threaded — `cargo test` runs each integration
//! test binary on its own thread, and the `PGPASSWORD` variable is only
//! touched here, so the safety invariant holds for the duration of the
//! test.

#![allow(unsafe_code)]

use narwhal_config::password_from_env;

/// Both env-var assertions live in one test so the surrounding
/// process-global `PGPASSWORD` set/remove can't race other tests
/// running in parallel. Splitting these into two `#[test]` functions
/// made them flake under cargo's default thread pool.
#[test]
fn env_var_resolution_round_trip() {
    // SAFETY: single-threaded test; no other thread touches PGPASSWORD.
    unsafe { std::env::set_var("PGPASSWORD", "from-env") };
    let pw = password_from_env("postgres");
    assert_eq!(pw.as_deref(), Some("from-env"));
    assert!(password_from_env("sqlite").is_none());

    // SAFETY: see above.
    unsafe { std::env::set_var("PGPASSWORD", "") };
    assert!(password_from_env("postgres").is_none());
    // SAFETY: see above.
    unsafe { std::env::remove_var("PGPASSWORD") };
    assert!(password_from_env("postgres").is_none());
}
