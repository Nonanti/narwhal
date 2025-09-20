use std::path::PathBuf;

use directories::ProjectDirs;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum PathsError {
    #[error("could not determine user config directories")]
    NoUserDirs,
}

/// Resolved on-disk paths used by narwhal.
///
/// On Linux this looks like:
///   config:  ~/.config/narwhal/
///   data:    ~/.local/share/narwhal/
///   cache:   ~/.cache/narwhal/
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

    pub fn log_dir(&self) -> PathBuf {
        self.cache_dir.join("logs")
    }

    /// Create all required directories on disk.
    pub fn ensure(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.config_dir)?;
        std::fs::create_dir_all(&self.data_dir)?;
        std::fs::create_dir_all(&self.cache_dir)?;
        std::fs::create_dir_all(self.log_dir())?;
        Ok(())
    }
}
