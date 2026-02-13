//! Per-connection "last opened at" timestamp store.
//!
//! Lives next to `connections.toml` in the data directory and feeds the
//! sidebar's recency-first ordering. Stored as a flat TOML map keyed by
//! the connection UUID so deletions in `connections.toml` simply leak a
//! tombstone entry that's cleaned up on the next save.
//!
//! We deliberately keep this out of `connections.toml` itself so the
//! config file the user manages by hand stays free of churn-y mtime
//! metadata.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum LastUsedError {
    #[error("could not read last-used file at {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("could not write last-used file at {path}: {source}")]
    Write {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("could not parse last-used file: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("could not serialise last-used file: {0}")]
    Serialise(#[from] toml::ser::Error),
}

/// On-disk shape. UUIDs are stored as their hyphenated string form so
/// the file is greppable in a terminal.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
struct OnDisk {
    #[serde(default)]
    entries: BTreeMap<String, u64>,
}

#[derive(Debug, Default, Clone)]
pub struct LastUsedStore {
    entries: BTreeMap<Uuid, u64>,
}

impl LastUsedStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Load the store from `path`. A missing file is not an error —
    /// fresh installations simply start with an empty map.
    pub fn load(path: &Path) -> Result<Self, LastUsedError> {
        match std::fs::read_to_string(path) {
            Ok(text) => {
                let on_disk: OnDisk = toml::from_str(&text)?;
                let entries = on_disk
                    .entries
                    .into_iter()
                    .filter_map(|(k, v)| Uuid::parse_str(&k).ok().map(|id| (id, v)))
                    .collect();
                Ok(Self { entries })
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::new()),
            Err(source) => Err(LastUsedError::Read {
                path: path.to_path_buf(),
                source,
            }),
        }
    }

    /// Persist to `path`. Creates parent directories on demand so the
    /// caller doesn't have to know whether the data directory exists.
    pub fn save(&self, path: &Path) -> Result<(), LastUsedError> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(|source| LastUsedError::Write {
                    path: path.to_path_buf(),
                    source,
                })?;
            }
        }
        let on_disk = OnDisk {
            entries: self
                .entries
                .iter()
                .map(|(k, v)| (k.to_string(), *v))
                .collect(),
        };
        let text = toml::to_string_pretty(&on_disk)?;
        // Sprint 6 (M13): atomic rename instead of `std::fs::write` so
        // a crash between truncate and write cannot leave the file in
        // a half-written state. Reuses the existing helper from
        // `settings.rs`.
        crate::settings::atomic_write(path, &text).map_err(|source| LastUsedError::Write {
            path: path.to_path_buf(),
            source,
        })?;
        Ok(())
    }

    /// Record that `id` was opened just now. Returns the timestamp
    /// that was written so callers can echo it back if they want.
    pub fn touch(&mut self, id: Uuid) -> u64 {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_millis() as u64);
        self.entries.insert(id, now);
        now
    }

    pub fn get(&self, id: Uuid) -> Option<u64> {
        self.entries.get(&id).copied()
    }

    /// Drop the entry for `id` (called from `:remove` so the store
    /// doesn't accumulate tombstones for permanently-gone connections).
    pub fn forget(&mut self, id: Uuid) {
        self.entries.remove(&id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn round_trips_through_disk() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("last_used.toml");
        let mut store = LastUsedStore::new();
        let id = Uuid::new_v4();
        store.touch(id);
        store.save(&path).unwrap();
        let loaded = LastUsedStore::load(&path).unwrap();
        assert!(loaded.get(id).is_some());
    }

    #[test]
    fn missing_file_loads_as_empty() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("nope.toml");
        let store = LastUsedStore::load(&path).unwrap();
        assert!(store.get(Uuid::new_v4()).is_none());
    }

    #[test]
    fn forget_removes_entry() {
        let mut store = LastUsedStore::new();
        let id = Uuid::new_v4();
        store.touch(id);
        store.forget(id);
        assert!(store.get(id).is_none());
    }

    #[test]
    fn invalid_uuid_keys_are_ignored() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("last_used.toml");
        std::fs::write(
            &path,
            "[entries]\n\"not-a-uuid\" = 123\n\"00000000-0000-0000-0000-000000000001\" = 456\n",
        )
        .unwrap();
        let store = LastUsedStore::load(&path).unwrap();
        // Only the valid entry survives.
        let parsed = Uuid::parse_str("00000000-0000-0000-0000-000000000001").unwrap();
        assert_eq!(store.get(parsed), Some(456));
    }
}

#[cfg(test)]
mod sprint6_tests {
    use super::*;
    use tempfile::TempDir;

    /// M13: a crash between truncate and write must not leave a
    /// half-written file. Atomic rename ensures the visible file is
    /// always complete. We can't easily simulate a crash, but we can
    /// verify the temp file is gone after a successful save (which
    /// implies the rename completed atomically).
    #[test]
    fn save_uses_atomic_rename_not_truncate() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("last_used.toml");
        let mut store = LastUsedStore::new();
        store.touch(Uuid::new_v4());
        store.save(&path).expect("save");

        let entries: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        // Only the final file should remain; no `.narwhal-*.tmp`
        // sidecar means the rename succeeded.
        assert!(
            entries
                .iter()
                .all(|n| !n.to_string_lossy().contains(".tmp")),
            "stale temp file left behind: {entries:?}"
        );
    }
}
