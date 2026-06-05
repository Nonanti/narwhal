use std::path::PathBuf;

use directories::ProjectDirs;
use thiserror::Error;

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum PathsError {
    #[error("could not determine user directories")]
    NoUserDirs,
}

/// On-disk locations used by narwhal.
///
/// On Linux this resolves to `~/.config/narwhal`, `~/.local/share/narwhal`
/// and `~/.cache/narwhal`. The macOS and Windows resolutions follow
/// platform conventions and are delegated to `directories`.
#[derive(Debug, Clone)]
pub struct ConfigPaths {
    pub config_dir: PathBuf,
    pub data_dir: PathBuf,
    pub cache_dir: PathBuf,
}

impl ConfigPaths {
    pub fn discover() -> Result<Self, PathsError> {
        let dirs = ProjectDirs::from("dev", "narwhal", "narwhal").ok_or(PathsError::NoUserDirs)?;
        Ok(Self {
            config_dir: dirs.config_dir().to_path_buf(),
            data_dir: dirs.data_dir().to_path_buf(),
            cache_dir: dirs.cache_dir().to_path_buf(),
        })
    }

    pub fn settings_file(&self) -> PathBuf {
        self.config_dir.join("config.toml")
    }

    pub fn connections_file(&self) -> PathBuf {
        self.config_dir.join("connections.toml")
    }

    pub fn history_file(&self) -> PathBuf {
        self.data_dir.join("history.jsonl")
    }

    /// Per-connection last-used timestamp store. Lives in the *data*
    /// directory rather than the cache dir so distro-managed cache
    /// wipes don't lose recently-touched-first ordering across reboots.
    pub fn last_used_file(&self) -> PathBuf {
        self.data_dir.join("last_used.toml")
    }

    /// T1-T3-B: workspace-state snapshot (open tabs, cursor/scroll,
    /// sidebar, active connection). Lives in the *config* directory
    /// alongside `settings.toml` and `connections.toml` so users who
    /// version their `~/.config/narwhal` see workspace restore as a
    /// first-class config artefact rather than something hidden under
    /// `~/.local/share`. The file format is TOML, matching every other
    /// narwhal-managed file in this directory.
    pub fn workspace_state_file(&self) -> PathBuf {
        self.config_dir.join("workspace-state.toml")
    }

    pub fn log_dir(&self) -> PathBuf {
        self.cache_dir.join("logs")
    }

    /// Directory scanned at start-up for auto-loaded Lua plugins. Every
    /// `*.lua` file at the top level of this directory is registered
    /// before the TUI takes over the screen.
    pub fn plugins_dir(&self) -> PathBuf {
        self.config_dir.join("plugins")
    }

    /// Create every directory referenced by this struct.
    ///
    /// On failure the returned error mentions the offending path so the
    /// user can tell which mount-point / permission is the problem
    /// (L37). Without the wrapping a bare `Permission denied` was
    /// impossible to debug.
    pub fn ensure(&self) -> std::io::Result<()> {
        fn mk(path: &std::path::Path) -> std::io::Result<()> {
            std::fs::create_dir_all(path).map_err(|e| {
                std::io::Error::new(
                    e.kind(),
                    format!("could not create {}: {e}", path.display()),
                )
            })
        }
        mk(&self.config_dir)?;
        mk(&self.data_dir)?;
        mk(&self.cache_dir)?;
        mk(&self.log_dir())?;
        mk(&self.plugins_dir())?;
        Ok(())
    }
}
