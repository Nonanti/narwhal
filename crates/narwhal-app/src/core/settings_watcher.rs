//! Live-reload watcher for `settings.toml`.
//!
//! When the user edits `~/.config/narwhal/config.toml` from another
//! editor (vim, VS Code, ...), this watcher spots the change and
//! forwards it to the main run loop, which re-parses the file and
//! calls `AppCore::apply_settings`. The settings modal's own save
//! path emits a "self-write" suppression token so the watcher
//! doesn't double-apply changes the modal already wired in.
//!
//! The watcher runs on a dedicated `notify` thread (the crate's
//! default backend), pushing into a tokio mpsc that the main loop
//! drains alongside `RunUpdate` / `MetaUpdate`.

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use notify::{Config, Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use tokio::sync::mpsc;

/// Errors that can occur when spawning the settings watcher.
#[derive(Debug, thiserror::Error)]
pub enum SettingsWatcherError {
    /// The supplied path has no file-name component (e.g. `/`).
    #[error("settings path has no file name: {0}")]
    NoFileName(std::path::PathBuf),
    /// Underlying `notify` error.
    #[error(transparent)]
    Notify(#[from] notify::Error),
}

/// Update emitted by the watcher.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum SettingsUpdate {
    /// The watched file changed. Carries no payload — the consumer
    /// re-reads `settings.toml` on its own so a corrupt write isn't
    /// echoed back as a parse error from inside the watcher
    /// thread.
    Changed,
}

/// Handle to the watcher. Drops on shutdown to stop the background
/// thread.
pub struct SettingsWatcher {
    _watcher: RecommendedWatcher,
}

impl SettingsWatcher {
    /// Start watching `path` for any modification or rename event.
    /// `tx` is signalled at most once per quiet window (250 ms) so a
    /// flurry of intermediate writes from a save-pass-through editor
    /// (vim's `:w` writes a `.swp`, then renames) collapses into a
    /// single reload trigger.
    ///
    /// Returns the watcher handle plus the receiver the run loop
    /// drains. The watcher stops when the handle is dropped.
    pub fn spawn(
        path: &Path,
    ) -> Result<(Self, mpsc::Receiver<SettingsUpdate>), SettingsWatcherError> {
        let (tx, rx) = mpsc::channel::<SettingsUpdate>(8);
        let last_emit: Arc<Mutex<Option<Instant>>> = Arc::new(Mutex::new(None));
        let tx_for_handler = tx.clone();

        // Capture the file name so the callback can reject sibling-file events
        // (connections.toml, workspace-state.toml, sessions/*.toml, …) that
        // arrive because we watch the parent directory for atomic rename.
        let settings_file_name = path
            .file_name()
            .map(|n| n.to_os_string())
            .ok_or_else(|| SettingsWatcherError::NoFileName(path.to_path_buf()))?;

        let mut watcher = RecommendedWatcher::new(
            move |res: Result<Event, notify::Error>| {
                let Ok(event) = res else {
                    return;
                };
                if !matches!(
                    event.kind,
                    EventKind::Modify(_) | EventKind::Create(_) | EventKind::Remove(_)
                ) {
                    return;
                }
                // Only react to paths whose file name matches the watched
                // settings file — ignore all sibling changes.
                let matches = event
                    .paths
                    .iter()
                    .any(|p| p.file_name() == Some(settings_file_name.as_os_str()));
                if !matches {
                    return;
                }
                // Coalesce burst writes (vim's swap-then-rename).
                let Ok(mut guard) = last_emit.lock() else {
                    return;
                };
                let now = Instant::now();
                if guard.is_some_and(|t| now.duration_since(t) < Duration::from_millis(250)) {
                    return;
                }
                *guard = Some(now);
                drop(guard);
                let _ = tx_for_handler.try_send(SettingsUpdate::Changed);
            },
            Config::default(),
        )?;
        // Watch the parent directory rather than the file itself so
        // atomic-rename saves (write tmp + rename) still fire.
        if let Some(parent) = path.parent() {
            watcher.watch(parent, RecursiveMode::NonRecursive)?;
        } else {
            watcher.watch(path, RecursiveMode::NonRecursive)?;
        }
        Ok((Self { _watcher: watcher }, rx))
    }
}
