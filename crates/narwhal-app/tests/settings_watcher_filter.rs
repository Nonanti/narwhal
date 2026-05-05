//! Regression test: the settings watcher must ignore changes to sibling
//! files in the same directory (e.g. connections.toml, workspace-state.toml)
//! and only react when the watched settings file itself changes.
//!
//! Why this is `#[ignore]`-d on macOS: `tempfile::tempdir()` returns a
//! path under `/var/folders/.../T/`, which is the macOS
//! `_CS_DARWIN_USER_TEMP_DIR`. `FsEventWatcher` (the
//! `RecommendedWatcher` on macOS) has documented
//! delivery quirks in that directory family on CI runners — events for
//! files created *after* the watcher is attached are sometimes batched
//! or dropped under load, producing flaky timeouts. Production paths
//! (`~/.config/narwhal/`, `~/Library/Application Support/narwhal/`)
//! are not affected, and Linux inotify delivers reliably. The test
//! still runs on Linux CI where it has caught a real regression.
//!
//! macOS users who want to validate locally can run
//! `cargo test --test settings_watcher_filter -- --ignored` from a
//! quiet shell.

use std::time::Duration;

use narwhal_app::core::settings_watcher::SettingsWatcher;
use tempfile::tempdir;
use tokio::time::timeout;

#[cfg_attr(
    target_os = "macos",
    ignore = "flaky on macOS CI: FsEventWatcher batches/drops events in /var/folders"
)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn settings_watcher_ignores_sibling_files() {
    let dir = tempdir().expect("tempdir creation failed");
    let cfg = dir.path().join("config.toml");
    std::fs::write(&cfg, "").expect("initial config write failed");

    let (_watcher, mut rx) = SettingsWatcher::spawn(&cfg).expect("watcher spawn failed");

    // Give the watcher a moment to settle after initial file creation.
    // macOS FSEvents has a longer initial delivery latency than
    // inotify (often ~500 ms on CI runners), so we wait longer there.
    let settle = if cfg!(target_os = "macos") { 1500 } else { 300 };
    tokio::time::sleep(Duration::from_millis(settle)).await;

    // Drain any leftover event from the initial write. macOS may
    // coalesce several FSEvents into one notification batch, so loop
    // until the queue is empty.
    while rx.try_recv().is_ok() {}

    // Write to a sibling file — must NOT trigger a settings reload.
    // The wait must be long enough that a real sibling-driven event
    // would have arrived on a quiet runner; macOS FSEvents bunches
    // events on a ~1 s timer so we give it 2 s of headroom.
    std::fs::write(dir.path().join("connections.toml"), "noise").expect("sibling write failed");
    let negative_wait = if cfg!(target_os = "macos") { 2000 } else { 500 };
    tokio::time::sleep(Duration::from_millis(negative_wait)).await;
    assert!(
        rx.try_recv().is_err(),
        "watcher must not fire for sibling file changes"
    );

    // Write to the actual settings file — MUST trigger a reload.
    // 10 s upper bound accommodates FSEvents on a heavily loaded
    // macOS CI runner (inotify on Linux typically delivers in <50 ms).
    std::fs::write(&cfg, "# real change").expect("config write failed");
    let received = timeout(Duration::from_secs(10), rx.recv())
        .await
        .expect("timed out waiting for settings change event");
    assert!(
        received.is_some(),
        "watcher should emit Changed for the watched file"
    );
}
