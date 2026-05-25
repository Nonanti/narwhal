use std::path::Path;

use narwhal_core::{ConnectionConfig, SslMode};
use serde::{Deserialize, Serialize};

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ConfigError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("toml parse: {0}")]
    Toml(#[from] toml::de::Error),
    #[error("toml serialize: {0}")]
    TomlSer(#[from] toml::ser::Error),
    #[error("validation: {0}")]
    Validation(String),
    #[error("interpolation: {0}")]
    Interpolate(String),
}

use thiserror::Error;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
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
    /// Per-group keymap overrides. Keys are group names
    /// (`results`, `row-detail`, ...), values map a chord string
    /// (`"ctrl+s"`, `"K"`, ...) to an action name
    /// (`"results-commit-pending"`). See the `narwhal-commands::keymap`
    /// crate for the full vocabulary. Unknown chords or actions surface
    /// at start-up as a status-bar warning; the rest of the bindings
    /// still load.
    ///
    /// L36 introduced this section. Empty by default; the built-in keymap
    /// continues to apply for every chord the user has not overridden.
    #[serde(default)]
    pub keymap: std::collections::HashMap<String, std::collections::HashMap<String, String>>,
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
        atomic_write(path, &text).map_err(ConfigError::from)?;
        Ok(())
    }
}

impl ConnectionsFile {
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = std::fs::read_to_string(path)?;
        let mut file: Self = toml::from_str(&text)?;
        // L36 #6: expand `${env:VAR}` placeholders in every string
        // field before validation — missing variables surface as a
        // ConfigError instead of a confusing engine-level connect
        // failure later on.
        crate::interpolate::interpolate_connections(&mut file)
            .map_err(|e| ConfigError::Interpolate(e.to_string()))?;
        validate_connections(&file.connections)?;
        Ok(file)
    }

    pub fn save(&self, path: &Path) -> Result<(), ConfigError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        // Validate before writing so corrupt configs are never persisted.
        validate_connections(&self.connections)?;
        let text = toml::to_string_pretty(self)?;
        atomic_write(path, &text).map_err(ConfigError::from)?;
        Ok(())
    }

    /// Parse a TOML string directly (useful for tests). Skips
    /// `${env:…}` interpolation so test fixtures can assert against
    /// the raw placeholder text without setting up process env vars.
    pub fn load_from_str(toml: &str) -> Result<Self, ConfigError> {
        let file: Self = toml::from_str(toml)?;
        validate_connections(&file.connections)?;
        Ok(file)
    }
}

impl Settings {
    /// Parse a TOML string into a [`Settings`] value. Companion to
    /// [`Self::load`] for test fixtures and other in-memory callers
    /// that already have the file contents in hand.
    pub fn load_from_str(text: &str) -> Result<Self, ConfigError> {
        let s: Self = toml::from_str(text)?;
        Ok(s)
    }
}

/// Write `data` to `path` atomically by writing to a temporary file
/// in the same directory and renaming. This prevents partial writes
/// from corrupting the config file on crash or power loss.
pub(crate) fn atomic_write(path: &Path, data: &str) -> std::io::Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let temp_name = format!(
        ".narwhal-{}.tmp",
        path.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("config")
    );
    let temp_path = parent.join(temp_name);
    std::fs::write(&temp_path, data)?;
    // Sprint 6 (LOW): tighten permissions on unix so a config file
    // that may carry interpolated `${env:VAR}` references or
    // connection metadata is not world-readable. We set the mode on
    // the temp file *before* rename so the visible file is never
    // briefly readable by others.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o600);
        // Best-effort: ignore failure on filesystems that don't honour
        // POSIX modes (FAT, some network FS) so the rename below can
        // still complete.
        let _ = std::fs::set_permissions(&temp_path, perms);
    }
    std::fs::rename(&temp_path, path)?;
    Ok(())
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
    // L5: catch duplicate UUIDs early. The keyring uses `id` as the key,
    // so two configs sharing an id silently collide on credentials.
    let mut seen_ids: std::collections::HashMap<uuid::Uuid, &str> =
        std::collections::HashMap::with_capacity(connections.len());
    for conn in connections {
        if let Some(prior) = seen_ids.insert(conn.id, conn.name.as_str()) {
            return Err(ConfigError::Validation(format!(
                "connections '{prior}' and '{}' share id {}",
                conn.name, conn.id
            )));
        }
    }
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
