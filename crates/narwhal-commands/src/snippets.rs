//! Snippet store — persisted named queries.
//!
//! Each saved query lives in its own `<name>.sql` file under the snippet
//! root directory (`~/.config/narwhal/snippets/` by default). The
//! directory is created lazily on first save. Names are restricted to
//! lowercase letters, digits, dashes, and underscores so the directory
//! stays portable across filesystems.

use std::path::PathBuf;

/// Errors produced by [`SnippetStore`] operations.
#[derive(Debug, thiserror::Error)]
pub enum SnippetError {
    /// The snippet name contains characters outside the allowed set.
    #[error("invalid snippet name '{0}': use lowercase letters, digits, '-', or '_' only")]
    InvalidName(String),
    /// An I/O error occurred.
    #[error("{0}")]
    Io(#[from] std::io::Error),
}

/// Result type for snippet operations.
pub type Result<T> = std::result::Result<T, SnippetError>;

/// Persistent store for named query snippets.
///
/// Each snippet is stored as `<root>/<name>.sql`. The root directory is
/// created lazily on the first [`SnippetStore::save`] call.
pub struct SnippetStore {
    /// Filesystem root that holds `<name>.sql` files.
    pub root: PathBuf,
}

impl SnippetStore {
    /// Create a new store pointing at `root`.
    pub const fn new(root: PathBuf) -> Self {
        Self { root }
    }

    /// Determine the default snippet root, respecting `XDG_CONFIG_HOME`.
    ///
    /// Uses the same `directories::ProjectDirs` crate that the rest of
    /// narwhal uses, so `XDG_CONFIG_HOME` is handled consistently.
    /// Falls back to `~/.config/narwhal/snippets/` if `ProjectDirs` cannot
    /// be resolved.
    pub fn default_root() -> PathBuf {
        directories::ProjectDirs::from("dev", "narwhal", "narwhal").map_or_else(
            || PathBuf::from(".").join("narwhal").join("snippets"),
            |dirs| dirs.config_dir().join("snippets"),
        )
    }

    /// Save `sql` under `name`. Overwrites if the name already exists.
    /// Creates the root directory lazily on first write.
    ///
    /// Uses write-then-rename so the final `.sql` file is never
    /// partially written: if the process crashes between the write and
    /// the rename, only the `.tmp` file is left behind and the previous
    /// content is intact. `rename` is atomic on POSIX within the same
    /// filesystem.
    pub fn save(&self, name: &str, sql: &str) -> Result<()> {
        Self::validate_name(name)?;
        std::fs::create_dir_all(&self.root)?;
        let tmp_path = self.root.join(format!(".{name}.sql.tmp"));
        let final_path = self.root.join(format!("{name}.sql"));
        std::fs::write(&tmp_path, sql)?;
        std::fs::rename(&tmp_path, &final_path)?;
        Ok(())
    }

    /// Load the SQL content for `name`.
    pub fn load(&self, name: &str) -> Result<String> {
        Self::validate_name(name)?;
        let path = self.root.join(format!("{name}.sql"));
        Ok(std::fs::read_to_string(path)?)
    }

    /// Remove the snippet file for `name`.
    pub fn remove(&self, name: &str) -> Result<()> {
        Self::validate_name(name)?;
        let path = self.root.join(format!("{name}.sql"));
        std::fs::remove_file(path)?;
        Ok(())
    }

    /// List all snippet names, sorted alphabetically.
    ///
    /// Returns an empty `Vec` if the root directory does not exist yet.
    pub fn list(&self) -> Result<Vec<String>> {
        if !self.root.exists() {
            return Ok(Vec::new());
        }
        let mut names: Vec<String> = std::fs::read_dir(&self.root)?
            .filter_map(|entry| {
                let entry = entry.ok()?;
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) == Some("sql") {
                    path.file_stem()?
                        .to_str()
                        .map(std::borrow::ToOwned::to_owned)
                } else {
                    None
                }
            })
            .collect();
        names.sort();
        Ok(names)
    }

    /// Validate that `name` is non-empty and contains only allowed
    /// characters: lowercase ASCII letters, digits, `-`, and `_`.
    fn validate_name(name: &str) -> Result<()> {
        let ok = !name.is_empty()
            && name
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_');
        if ok {
            Ok(())
        } else {
            Err(SnippetError::InvalidName(name.into()))
        }
    }
}
