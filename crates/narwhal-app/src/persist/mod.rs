//! Workspace persistence.
//!
//! Snapshots the user's open tabs, cursor/scroll positions, sidebar
//! state and active connection on clean exit; restores them on the
//! next launch when `[settings.workspace.persist]` opts in (the v2.0
//! defaults all opt in).
//!
//! ## Wire format
//!
//! The file at
//! `~/.config/narwhal/workspace-state.toml`
//! (see [`paths::default_workspace_state_path`]) is a TOML document
//! whose top-level shape mirrors [`schema::PersistedWorkspace`]. The
//! first key is always `schema_version`, matching the precedent set
//! by `settings.toml` / `connections.toml`. Empty optionals collapse
//! on serialisation so a fresh "just one untitled tab" install
//! produces a 4-line file rather than a 30-line one.
//!
//! ## Save triggers
//!
//! - **Clean exit only**: [`save_at_exit`] is called from the event
//! loop after the main loop has terminated normally. Panic
//! unwinds skip persistence to avoid serialising a half-mutated
//! state.
//! - **No throttled background save in v2.0**: the brief suggested
//! one every 30s; we defer it until there's evidence of unclean
//! shutdowns in the wild. The atomic-rename guarantees the file
//! is either pre-snapshot or fully-current — never half-written.
//!
//! ## Load triggers
//!
//! [`load_at_start`] runs from the binary's `App::with_workspace_state_path`
//! during construction so the very first frame reflects the restored
//! sidebar / tab list. Connection restore is asynchronous and happens
//! after the event loop spins up; see
//! `restore_workspace_after_startup` in `core/persist_hook.rs`.
//!
//! ## Concurrent narwhal instances
//!
//! Two narwhal processes pointed at the same `workspace-state.toml`
//! would race the rename. [`paths::acquire_lock`] coordinates them
//! with a `.lock` sibling file (POSIX-atomic `create_new`). On
//! contention the would-be writer falls back to
//! `workspace-state.${pid}.toml` so neither instance loses its
//! snapshot. Stale locks older than `paths::STALE_LOCK_SECS` are
//! reaped on the next save.
//!
//! ## Privacy
//!
//! Editor buffers are serialised verbatim. Users who paste secrets
//! into the editor (`SELECT * FROM users WHERE token = '...'`) get
//! those secrets written to disk in plaintext. The file is created
//! with `0o600` on Unix. Users who need an encrypted location can
//! disable persistence via `settings.workspace.persist.enabled =
//! false` or point `XDG_CONFIG_HOME` at an encrypted directory.

use std::path::{Path, PathBuf};

use narwhal_config::WorkspacePersistSettings;

use crate::core::AppCore;

pub mod paths;
pub mod schema;

pub use schema::{
    CURRENT_SCHEMA_VERSION, PersistedSidebar, PersistedTab, PersistedTabKind, PersistedWorkspace,
};

/// All-encompassing error surface for the persist layer.
///
/// Variants surface to the host as `tracing` warnings; restore never
/// fails the launch, and save never fails the shutdown. The error
/// type stays public so tests (and future MCP / CLI tooling that
/// wants to introspect snapshots) can pattern-match on the cause.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum PersistError {
    /// I/O while reading or writing the snapshot file.
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    /// TOML decode (malformed file, e.g. user edited it by hand).
    #[error("toml decode: {0}")]
    TomlDecode(#[from] toml::de::Error),
    /// TOML encode. Practically unreachable for the persisted
    /// shapes (every leaf is `Default` and serialisable) but kept
    /// for completeness so callers don't have to guess.
    #[error("toml encode: {0}")]
    TomlEncode(#[from] toml::ser::Error),
    /// Snapshot's `schema_version` is newer than what this binary
    /// supports. Restore skips with a warning; the file is left
    /// untouched so a newer narwhal can still read it.
    #[error(
        "workspace-state schema_version {found} is newer than supported {supported}; \
         skip restore (upgrade narwhal)"
    )]
    UnsupportedSchema { found: u32, supported: u32 },
}

/// Convenience alias used inside the persist layer.
pub type PersistResult<T> = Result<T, PersistError>;

/// Read a snapshot file, returning `Ok(None)` when the file does not
/// exist (the first-run case). Schema-mismatched and malformed files
/// surface as errors so the caller can decide between "warn and
/// skip" (the production path) and "fail the test fixture" (the
/// test path).
pub fn load_at_start(path: &Path) -> PersistResult<Option<PersistedWorkspace>> {
    if !path.exists() {
        return Ok(None);
    }
    let text = std::fs::read_to_string(path)?;
    let snapshot: PersistedWorkspace = toml::from_str(&text)?;
    if snapshot.schema_version > CURRENT_SCHEMA_VERSION {
        return Err(PersistError::UnsupportedSchema {
            found: snapshot.schema_version,
            supported: CURRENT_SCHEMA_VERSION,
        });
    }
    Ok(Some(snapshot))
}

/// Serialise `snapshot` and write it atomically to `path`.
///
/// Tries to acquire the cross-process lock first; on contention,
/// falls back to a per-pid path returned via [`SaveOutcome`].
/// Surfaces I/O errors so the caller can log them — production code
/// downgrades the error to a `tracing::warn`, tests assert on the
/// returned outcome.
pub fn save_at_exit(snapshot: &PersistedWorkspace, path: &Path) -> PersistResult<SaveOutcome> {
    paths::ensure_parent(path)?;
    let text = toml::to_string_pretty(snapshot)?;
    match paths::acquire_lock(path)? {
        paths::LockOutcome::Owned(guard) => {
            paths::atomic_write(path, &text)?;
            guard.release()?;
            Ok(SaveOutcome::Canonical(path.to_path_buf()))
        }
        paths::LockOutcome::Contended => {
            // Another narwhal instance owns the canonical file.
            // Fall back to a per-pid sibling so neither instance
            // loses its snapshot. The next clean exit that
            // succeeds in acquiring the lock takes over the
            // canonical slot; the per-pid file is left in place as
            // a recoverable last-resort copy.
            let pid_path = paths::per_pid_path(path, std::process::id());
            paths::atomic_write(&pid_path, &text)?;
            Ok(SaveOutcome::PerPid(pid_path))
        }
    }
}

/// What [`save_at_exit`] ended up writing.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum SaveOutcome {
    /// Wrote the canonical `workspace-state.toml`.
    Canonical(PathBuf),
    /// Lock was contended; wrote `workspace-state.${pid}.toml`
    /// instead. Caller should `tracing::info!` the path so the
    /// operator can find the file if they need to recover from it.
    PerPid(PathBuf),
}

/// Snapshot the current [`AppCore`] state into a wire-format
/// [`PersistedWorkspace`].
///
/// The projection is non-destructive: it only reads. Disabled
/// persistence still allows callers to take a snapshot (useful for
/// "remember-the-session" features) — the gating happens at the
/// [`save_at_exit`] call-site, not here.
pub fn snapshot(core: &AppCore) -> PersistedWorkspace {
    crate::core::persist_hook::project_workspace(core)
}

/// Apply a previously-loaded snapshot onto a freshly-constructed
/// [`AppCore`]. Honours the per-knob restore flags so a user that
/// only wants the connection restored doesn't also get their tabs
/// thrust back into existence.
///
/// Returns the name of the active connection that should be
/// re-opened asynchronously after the event loop spins up — the
/// caller fires `:open <name>` once the runtime is ready. Returns
/// `None` when restore is disabled or the snapshot had no active
/// connection.
pub fn apply(
    core: &mut AppCore,
    snapshot: PersistedWorkspace,
    settings: &WorkspacePersistSettings,
) -> Option<String> {
    crate::core::persist_hook::apply_workspace(core, snapshot, settings)
}
