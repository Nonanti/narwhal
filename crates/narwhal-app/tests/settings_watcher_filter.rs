//! Regression test: the settings watcher must ignore changes to sibling
//! files in the same directory (e.g. connections.toml, workspace-state.toml)
//! and only react when the watched settings file itself changes.

use std::time::Duration;

use narwhal_app::core::settings_watcher::SettingsWatcher;
use tempfile::tempdir;
use tokio::time::timeout;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn settings_watcher_ignores_sibling_files() {
    let dir = tempdir().expect("tempdir creation failed");
    let cfg = dir.path().join("config.toml");
    std::fs::write(&cfg, "").expect("initial config write failed");

    let (_watcher, mut rx) = SettingsWatcher::spawn(&cfg).expect("watcher spawn failed");

    // Give the watcher a moment to settle after initial file creation.
    tokio::time::sleep(Duration::from_millis(300)).await;

    // Drain any leftover event from the initial write.
    let _ = rx.try_recv();

    // Write to a sibling file — must NOT trigger a settings reload.
    std::fs::write(dir.path().join("connections.toml"), "noise").expect("sibling write failed");
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(
        rx.try_recv().is_err(),
        "watcher must not fire for sibling file changes"
    );

    // Write to the actual settings file — MUST trigger a reload.
    std::fs::write(&cfg, "# real change").expect("config write failed");
    let received = timeout(Duration::from_secs(3), rx.recv())
        .await
        .expect("timed out waiting for settings change event");
    assert!(
        received.is_some(),
        "watcher should emit Changed for the watched file"
    );
}
