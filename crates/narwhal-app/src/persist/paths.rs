//! Workspace-state path resolver, atomic writer, and best-effort lock
//! fallback. Keeps the disk plumbing out of `mod.rs` so the API layer
//! reads like a control-flow description.

use std::path::{Path, PathBuf};

use narwhal_config::{ConfigPaths, PathsError};

/// Resolve `~/.config/narwhal/workspace-state.toml`. Falls back to a
/// platform-appropriate location via [`directories::ProjectDirs`].
pub fn default_workspace_state_path() -> Result<PathBuf, PathsError> {
    Ok(ConfigPaths::discover()?.workspace_state_file())
}

/// Compute the per-pid sibling path used when a sibling narwhal
/// instance already owns the canonical workspace-state file. Layout is
/// `workspace-state.${pid}.toml` next to the canonical file.
pub fn per_pid_path(canonical: &Path, pid: u32) -> PathBuf {
    let stem = canonical
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("workspace-state");
    let ext = canonical
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("toml");
    let parent = canonical.parent().unwrap_or_else(|| Path::new("."));
    parent.join(format!("{stem}.{pid}.{ext}"))
}

/// Companion lock-file path. Sits next to the canonical state file
/// with a `.lock` suffix.
pub fn lock_path(canonical: &Path) -> PathBuf {
    let mut lock = canonical.as_os_str().to_owned();
    lock.push(".lock");
    PathBuf::from(lock)
}

/// Stale-lock cutoff. Lock files older than this on disk are treated
/// as orphans from a crashed earlier run and quietly reaped. One
/// minute is comfortably longer than any legitimate snapshot write
/// but short enough that the next clean exit re-establishes the
/// canonical path.
///
/// Public so the `persist` module-level documentation can link to it
/// without crossing a visibility boundary; the value is informational
/// only — callers should never need to read it.
pub const STALE_LOCK_SECS: u64 = 60;

/// Write `data` to `path` atomically by writing to a sibling temp
/// file and renaming. Mirrors `narwhal_config::settings::atomic_write`
/// so the persist module doesn't depend on the config crate's private
/// helpers.
///
/// `path`'s parent directory must already exist (callers run
/// [`ensure_parent`] first).
pub fn atomic_write(path: &Path, data: &str) -> std::io::Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let temp_name = format!(
        ".narwhal-{}.tmp",
        path.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("workspace-state")
    );
    let temp_path = parent.join(temp_name);
    std::fs::write(&temp_path, data)?;
    // Unix-only mode tightening: the snapshot may contain query
    // buffers with secrets pasted in by the user. The companion
    // settings.toml uses 0o600 for the same reason.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o600);
        // Best-effort: ignore failure on filesystems that don't
        // honour POSIX modes so the rename below can still proceed.
        let _ = std::fs::set_permissions(&temp_path, perms);
    }
    std::fs::rename(&temp_path, path)?;
    Ok(())
}

/// Create the parent directory of `path` if it does not already
/// exist.
pub fn ensure_parent(path: &Path) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    Ok(())
}

/// Outcome of `acquire_lock`.
///
/// `Owned` means the current process is the sole writer for this
/// path. `Contended` means another narwhal instance holds the lock
/// (and the caller should fall back to a per-pid file).
#[derive(Debug)]
pub enum LockOutcome {
    Owned(LockGuard),
    Contended,
}

/// RAII handle for the on-disk `.lock` sentinel. Dropping the guard
/// removes the lock file; explicit `release()` does the same and
/// surfaces I/O errors instead of swallowing them.
#[derive(Debug)]
pub struct LockGuard {
    path: PathBuf,
    released: bool,
}

impl LockGuard {
    pub fn release(mut self) -> std::io::Result<()> {
        self.released = true;
        match std::fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            // Already gone (e.g. cleaned up by another process that
            // detected a stale lock) — treat as success.
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(err),
        }
    }
}

impl Drop for LockGuard {
    fn drop(&mut self) {
        if !self.released {
            // Best-effort: drop-on-panic should never escalate to a
            // panic of its own.
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// Try to take exclusive ownership of `canonical` for the duration
/// of a snapshot write.
///
/// Strategy:
///
/// 1. Try `create_new` on the `.lock` sibling. POSIX guarantees the
///    create-and-fail-if-exists is atomic, which is enough for the
///    cross-process serialisation we need.
/// 2. If the lock already exists and is older than `STALE_LOCK_SECS`,
///    treat it as an orphan from a crashed run and reap it before
///    retrying once.
/// 3. Anything else surfaces as [`LockOutcome::Contended`] so the
///    caller falls back to the per-pid path.
pub fn acquire_lock(canonical: &Path) -> std::io::Result<LockOutcome> {
    let lock_file = lock_path(canonical);
    match try_create_lock(&lock_file) {
        Ok(()) => Ok(LockOutcome::Owned(LockGuard {
            path: lock_file,
            released: false,
        })),
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
            // Stale-lock recovery: a previous narwhal run that
            // panicked between create and remove left the sentinel
            // behind. Reap if older than the cutoff and retry once.
            if is_stale(&lock_file).unwrap_or(false) {
                let _ = std::fs::remove_file(&lock_file);
                match try_create_lock(&lock_file) {
                    Ok(()) => Ok(LockOutcome::Owned(LockGuard {
                        path: lock_file,
                        released: false,
                    })),
                    Err(_) => Ok(LockOutcome::Contended),
                }
            } else {
                Ok(LockOutcome::Contended)
            }
        }
        Err(err) => Err(err),
    }
}

fn try_create_lock(lock_file: &Path) -> std::io::Result<()> {
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(lock_file)?;
    // Stamp the pid into the lock so a curious operator can identify
    // the holder. Best-effort write; failure here doesn't invalidate
    // the lock semantically.
    let _ = writeln!(f, "{}", std::process::id());
    Ok(())
}

fn is_stale(lock_file: &Path) -> std::io::Result<bool> {
    let meta = std::fs::metadata(lock_file)?;
    let modified = meta.modified()?;
    let age = std::time::SystemTime::now()
        .duration_since(modified)
        .unwrap_or_default();
    Ok(age.as_secs() > STALE_LOCK_SECS)
}
