use std::path::Path;

use narwhal_core::{ConnectionConfig, SslMode};
use serde::{Deserialize, Serialize};

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("toml parse: {0}")]
    Toml(#[from] toml::de::Error),
    #[error("toml serialize: {0}")]
    TomlSer(#[from] toml::ser::Error),
    #[error("validation: {0}")]
    Validation(String),
}

use thiserror::Error;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Theme {
    #[default]
    Dark,
    Light,
    HighContrast,
}

/// User-facing settings persisted to `config.toml`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub theme: Theme,
    pub editor: EditorSettings,
    pub keybindings: KeybindingSettings,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct EditorSettings {
    pub tab_width: u8,
    pub use_spaces: bool,
    pub line_numbers: bool,
}

impl Default for EditorSettings {
    fn default() -> Self {
        Self {
            tab_width: 4,
            use_spaces: true,
            line_numbers: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct KeybindingSettings {
    pub vim_mode: bool,
}

impl Default for KeybindingSettings {
    fn default() -> Self {
        Self { vim_mode: true }
    }
}

/// Container for the persisted connection list.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ConnectionsFile {
    #[serde(rename = "connection", default)]
    pub connections: Vec<ConnectionConfig>,
}

impl Settings {
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = std::fs::read_to_string(path)?;
        let settings = toml::from_str(&text)?;
        Ok(settings)
    }

    pub fn save(&self, path: &Path) -> Result<(), ConfigError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let text = toml::to_string_pretty(self)?;
        std::fs::write(path, text)?;
        Ok(())
    }
}

impl ConnectionsFile {
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = std::fs::read_to_string(path)?;
        let file: ConnectionsFile = toml::from_str(&text)?;
        validate_connections(&file.connections)?;
        Ok(file)
    }

    pub fn save(&self, path: &Path) -> Result<(), ConfigError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let text = toml::to_string_pretty(self)?;
        std::fs::write(path, text)?;
        Ok(())
    }

    /// Parse a TOML string directly (useful for tests).
    pub fn load_from_str(toml: &str) -> Result<Self, ConfigError> {
        let file: ConnectionsFile = toml::from_str(toml)?;
        validate_connections(&file.connections)?;
        Ok(file)
    }
}

/// Validate TLS-related constraints across all connections:
///
/// - `verify-ca` / `verify-full` requires `ssl_root_cert` to be set.
/// - sqlite / duckdb drivers reject *explicit* TLS modes that imply an
///   actual handshake (`require`, `verify-ca`, `verify-full`).  The
///   defaults (`prefer`) and the explicit `disable` both pass — file-local
///   drivers ignore the field at the wire layer, and rejecting the
///   default `prefer` would break every pre-existing sqlite/duckdb
///   config that landed before TLS fields existed.
fn validate_connections(connections: &[ConnectionConfig]) -> Result<(), ConfigError> {
    for conn in connections {
        // ssl_cert and ssl_key must both be set or both absent.
        let has_cert = conn.params.ssl_cert.is_some();
        let has_key = conn.params.ssl_key.is_some();
        if has_cert != has_key {
            return Err(ConfigError::Validation(format!(
                "connection '{}': ssl_cert and ssl_key must both be set or both absent",
                conn.name
            )));
        }

        // M3: ssl_mode = disable contradicts having TLS files set.
        let has_tls_files = conn.params.ssl_root_cert.is_some()
            || conn.params.ssl_cert.is_some()
            || conn.params.ssl_key.is_some();
        if conn.params.ssl_mode == SslMode::Disable && has_tls_files {
            return Err(ConfigError::Validation(format!(
                "connection '{}': ssl_root_cert/ssl_cert/ssl_key set but ssl_mode = disable",
                conn.name
            )));
        }

        let is_file_driver = matches!(conn.driver.as_str(), "sqlite" | "duckdb");

        if is_file_driver
            && matches!(
                conn.params.ssl_mode,
                SslMode::Require | SslMode::VerifyCa | SslMode::VerifyFull
            )
        {
            return Err(ConfigError::Validation(format!(
                "connection '{}': ssl_mode must be 'disable' for the '{}' driver \
                 (file-local databases do not support TLS)",
                conn.name, conn.driver
            )));
        }

        let needs_root_cert = matches!(
            conn.params.ssl_mode,
            SslMode::VerifyCa | SslMode::VerifyFull
        );
        if needs_root_cert && conn.params.ssl_root_cert.is_none() {
            let mode_name = match conn.params.ssl_mode {
                SslMode::VerifyCa => "verify-ca",
                SslMode::VerifyFull => "verify-full",
                _ => "unknown",
            };
            return Err(ConfigError::Validation(format!(
                "connection '{}': ssl_mode='{}' requires ssl_root_cert to be set",
                conn.name, mode_name
            )));
        }
    }
    Ok(())
}
