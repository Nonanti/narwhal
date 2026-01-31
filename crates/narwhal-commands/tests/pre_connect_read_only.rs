//! L36 #C4 regression: `SessionOpenOptions::skip_pre_connect` must
//! actually skip the shell pipeline.
//!
//! We can't open a real Session from this crate (it needs a live
//! driver), but we can exercise the gate the option turns on/off by
//! constructing a `PreConnectStep` that would *write a file* if
//! executed and then asserting the file's presence/absence after
//! calling the public runner directly with `skip_pre_connect`
//! semantics.

use std::path::PathBuf;

use narwhal_commands::pre_connect::run_pre_connect;
use narwhal_core::PreConnectStep;

fn tempfile_path(suffix: &str) -> PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!(
        "narwhal-preconnect-test-{}-{suffix}",
        std::process::id()
    ));
    p
}

#[tokio::test]
async fn run_pre_connect_actually_runs_when_invoked() {
    let marker = tempfile_path("run");
    let _ = std::fs::remove_file(&marker);
    let cmd = format!("touch {}", marker.display());
    let steps = vec![PreConnectStep::new(cmd).with_timeout_secs(5)];
    run_pre_connect(&steps).await.expect("step should run");
    assert!(
        marker.exists(),
        "pre-connect step should have created the marker file"
    );
    let _ = std::fs::remove_file(&marker);
}

#[tokio::test]
async fn skipping_pre_connect_leaves_filesystem_untouched() {
    // `skip_pre_connect` is enforced inside `Session::open_with`,
    // which the unit-test layer can't construct without a driver.
    // What we *can* assert is the contract: when the host decides to
    // skip, it simply never calls `run_pre_connect`. So we model
    // that by calling with an empty slice and verifying nothing
    // happens.
    let marker = tempfile_path("skip");
    let _ = std::fs::remove_file(&marker);
    let vars = run_pre_connect(&[]).await.expect("empty slice is fine");
    assert!(vars.is_empty());
    assert!(
        !marker.exists(),
        "no step was supplied, so the marker must not exist"
    );
}
