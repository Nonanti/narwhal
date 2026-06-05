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
    /// File declares `schema_version = N` where N is newer than the
    /// current binary supports. Tells the user to upgrade narwhal
    /// rather than silently dropping unknown fields.
    #[error("unsupported config schema_version = {0}; upgrade narwhal to read this file")]
    UnsupportedSchema(u32),
    /// File declares `schema_version = 1` and the caller asked for
    /// canonical (v2) shape. Emitted by [`Settings::load`] /
    /// [`ConnectionsFile::load`] so the migrate-config CLI can be
    /// suggested in the error message.
    #[error("settings file is v1; run `narwhal migrate-config` to convert to v2 (file: {path})")]
    NeedsMigration { path: std::path::PathBuf },
}

/// Canonical schema version produced by the current binary.
pub const CURRENT_SCHEMA_VERSION: u32 = 2;

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

/// Icon set used by the in-terminal diagram widget.
///
/// Mirrors `narwhal_diagram::IconSet` but lives here so the config
/// crate does not have to depend on `narwhal-diagram`. The host app
/// converts at the seam.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum DiagramIcons {
    /// Safe everywhere; `[PK]`, `[FK]`, `[UK]`, `(!)` markers.
    #[default]
    Ascii,
    /// Nerd Font glyphs — requires a patched terminal font.
    Nerdfont,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct DiagramSettings {
    /// Glyph set used inside the TUI diagram modal. Has no effect on
    /// `:diagram export` output (Mermaid / DOT renderers always use
    /// ASCII since their downstream viewers do not render Nerd Font).
    pub icons: DiagramIcons,
}

/// User-facing settings persisted to `settings.toml`.
///
/// Wire format is **v2** (`schema_version = 2`) since narwhal v2.0.
/// [`Settings::load`] still accepts v1 input on a one-shot path,
/// returns [`ConfigError::NeedsMigration`] so the caller can prompt
/// for `narwhal migrate-config`.
///
/// The struct itself is wire-format-agnostic: it is the shape the
/// rest of the app interacts with. The on-disk v2 layout wraps it
/// in a `SettingsFile` envelope that carries `schema_version` at
/// the top level.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub theme: Theme,
    pub editor: EditorSettings,
    pub keybindings: KeybindingSettings,
    pub diagram: DiagramSettings,
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
    // ----- v2 additions ------------------------------------------------
    /// Workspace-state persistence knobs. v2.0 adds the contract; the
    /// actual restore logic lands in T1-T3-B.
    #[serde(default)]
    pub workspace: WorkspaceSettings,
    /// Secret-vault provider configuration. v2.0 only ships the
    /// schema + a `none` default; concrete providers (`HashiCorp`,
    /// `1Password`, AWS, Azure) land in T1-T2-B.
    #[serde(default)]
    pub vault: VaultSettings,
    /// Plugin runtime discovery. The Lua bridge already exists in
    /// v1.x; v2.0 adds the `wasm` sub-section for the upcoming
    /// `wasmtime` runtime (T1-T5-A).
    #[serde(default)]
    pub plugins: PluginSettings,
    /// Streaming-result tuning. T1-T4-A exposes `batch_size` and
    /// `stream_flush_ms` so users can trade UI redraw traffic
    /// against first-row latency without rebuilding.
    #[serde(default)]
    pub run: RunSettings,
}

/// Streaming-result tuning.
///
/// T1-T4-A (v2.0) introduced this section. Defaults match the
/// hard-coded v1.x behaviour (`batch_size = 64`) so unconfigured
/// users see no semantic change; the new `stream_flush_ms` knob
/// adds the *time*-based flush that v1.x lacked.
///
/// Both fields apply to the TUI run worker only
/// (`narwhal_app::run::run_stream`). The MCP `run_query` tool keeps
/// its own row cap (`limit`) and is unaffected.
///
/// A `stream_buffer` knob (bounding the in-flight row channel
/// between sync drivers and the async worker) was originally part
/// of this struct, but the driver-side wiring it would have
/// controlled does not yet exist. Re-add when the sync-driver mpsc
/// seam is configurable end-to-end — a v2 minor bump can land the
/// field non-breakingly because the struct is `#[non_exhaustive]`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
#[non_exhaustive]
pub struct RunSettings {
    /// Maximum rows accumulated in the worker before a
    /// `RunUpdate::RowsAppended` batch is flushed to the UI. Higher
    /// values reduce channel traffic; lower values reduce first-row
    /// latency. Default `64` matches v1.x.
    pub batch_size: usize,
    /// Time window (milliseconds) after which a partially-filled
    /// batch is flushed even when `batch_size` has not been reached.
    /// Drives the "row count ticks up live" UI behaviour for slow
    /// queries. `0` disables the time-based flush and reverts to
    /// pure size-based batching. Default `50`.
    pub stream_flush_ms: u64,
}

impl Default for RunSettings {
    fn default() -> Self {
        Self {
            batch_size: 64,
            stream_flush_ms: 50,
        }
    }
}

/// On-disk v2 envelope. [`Settings::load`] / [`Settings::save`]
/// translate to and from this wrapper; the rest of the workspace
/// only ever sees the inner [`Settings`]. `pub(crate)` so the
/// migrate module can render the same wire format without
/// duplicating the header logic.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct SettingsFile {
    pub schema_version: u32,
    #[serde(default)]
    pub settings: Settings,
}

impl Default for SettingsFile {
    fn default() -> Self {
        Self {
            schema_version: CURRENT_SCHEMA_VERSION,
            settings: Settings::default(),
        }
    }
}

/// Workspace persistence knobs. **Stub in v2.0** — the contract is
/// defined here so users can author their `settings.toml` against
/// it, but the actual tab/cursor restore loop is implemented by
/// T1-T3-B.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
#[non_exhaustive]
pub struct WorkspaceSettings {
    pub persist: WorkspacePersistSettings,
}

/// Workspace-state persistence toggles. Defaults are all `true` so a
/// fresh install gets the "reopen where I left off" behaviour without
/// any config-file editing — the only knob users need to flip is
/// `enabled = false` if they prefer the v1 stateless start-up.
///
/// T1-T3-B (v2.0) wired the actual save/restore loop in
/// `narwhal_app::persist`. The defaults landed there too so the disk
/// shape (`~/.config/narwhal/workspace-state.toml`) only appears for
/// users who don't explicitly opt out.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
#[non_exhaustive]
pub struct WorkspacePersistSettings {
    pub enabled: bool,
    pub restore_tabs: bool,
    pub restore_cursor: bool,
    pub restore_sidebar: bool,
}

impl Default for WorkspacePersistSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            restore_tabs: true,
            restore_cursor: true,
            restore_sidebar: true,
        }
    }
}

/// Vault provider settings. **Stub in v2.0** — only `default_provider
/// = "none"` is wired up; T1-T2-B implements the concrete providers.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
#[non_exhaustive]
pub struct VaultSettings {
    pub default_provider: VaultProvider,
    pub providers: VaultProviderSettings,
}

impl VaultSettings {
    /// Builder shim for `#[non_exhaustive]` external construction.
    /// Mirrors `ConnectionParams::with` (T0-02 convention) so callers
    /// in other crates and integration tests can build a value
    /// without unstable struct-update syntax.
    #[must_use]
    pub fn with(f: impl FnOnce(&mut Self)) -> Self {
        let mut s = Self::default();
        f(&mut s);
        s
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum VaultProvider {
    #[default]
    None,
    Hashicorp,
    Onepassword,
    Aws,
    Azure,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
#[non_exhaustive]
pub struct VaultProviderSettings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hashicorp: Option<HashicorpVaultSettings>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub onepassword: Option<OnePasswordVaultSettings>,
}

impl VaultProviderSettings {
    /// Builder shim, see [`VaultSettings::with`].
    #[must_use]
    pub fn with(f: impl FnOnce(&mut Self)) -> Self {
        let mut s = Self::default();
        f(&mut s);
        s
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
#[non_exhaustive]
pub struct HashicorpVaultSettings {
    /// Vault address (e.g. `${env:VAULT_ADDR}`). Interpolated like
    /// every other config string.
    #[serde(default)]
    pub address: Option<String>,
    /// Name of the environment variable that holds the Vault token.
    /// We never store the token in `settings.toml` directly — it has
    /// to round-trip through an env var the user controls.
    #[serde(default)]
    pub token_env: Option<String>,
    /// Optional Vault Enterprise namespace. Sent as the
    /// `X-Vault-Namespace` header. `None` selects the root namespace.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub namespace: Option<String>,
    /// HTTP timeout for a single resolve call. `None` falls back to
    /// the [`crate::vault::DEFAULT_HASHICORP_TIMEOUT_SECS`] (5 s).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_secs: Option<u64>,
}

impl HashicorpVaultSettings {
    /// Builder shim, see [`VaultSettings::with`].
    #[must_use]
    pub fn with(f: impl FnOnce(&mut Self)) -> Self {
        let mut s = Self::default();
        f(&mut s);
        s
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
#[non_exhaustive]
pub struct OnePasswordVaultSettings {
    #[serde(default)]
    pub account: Option<String>,
    #[serde(default)]
    pub service_account_token_env: Option<String>,
    /// Override for the `op` binary path. Test fixtures point this at
    /// a shell stub so CI does not need a real 1Password account.
    /// `None` defers to `op` on `$PATH`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub op_binary: Option<String>,
    /// CLI timeout per `op read` invocation. `None` falls back to
    /// [`crate::vault::DEFAULT_OP_TIMEOUT_SECS`] (10 s).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_secs: Option<u64>,
}

impl OnePasswordVaultSettings {
    /// Builder shim, see [`VaultSettings::with`].
    #[must_use]
    pub fn with(f: impl FnOnce(&mut Self)) -> Self {
        let mut s = Self::default();
        f(&mut s);
        s
    }
}

/// Plugin runtime discovery + capability defaults.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
#[non_exhaustive]
pub struct PluginSettings {
    /// Lua plugin directory. `None` means the host falls back to
    /// `$XDG_CONFIG_HOME/narwhal/plugins/lua` (the historical
    /// path).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lua_dir: Option<String>,
    /// WASM plugin runtime config. **Stub in v2.0** — `enabled =
    /// false` until T1-T5-A wires up `wasmtime`.
    #[serde(default)]
    pub wasm: WasmPluginSettings,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
#[non_exhaustive]
pub struct WasmPluginSettings {
    pub enabled: bool,
    /// Directory containing `.wasm` components. `None` falls back to
    /// `$XDG_CONFIG_HOME/narwhal/plugins/wasm`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dir: Option<String>,
    pub allow_fs_read: bool,
    pub allow_fs_write: bool,
    pub allow_net: bool,
    pub allow_env: bool,
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

/// One user-declared logical relation between two tables.
///
/// Parsed from `[[logical_relation]]` blocks in either
/// `connections.toml` (under the top level) or
/// `.narwhal/workspace.toml`. Host code converts these into
/// `narwhal_diagram::LogicalRelation` after validating the table /
/// column references against the live schema.
///
/// ```toml
/// [[logical_relation]]
/// connection  = "prod-db"           # required
/// from        = "events.user_id"    # qualified: [schema.]table.column
/// to          = "users.id"
/// cardinality = "many-to-one"       # default: "many-to-one"
/// note        = "no FK because of cross-shard pruning"
/// ```
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LogicalRelationConfig {
    /// Connection name (must match a `[[connection]]` entry).
    pub connection: String,
    /// Child / referencing side. `[schema.]table.column`. Composite
    /// relations use `from_columns`/`to_columns` instead (V1.1).
    #[serde(default)]
    pub from: Option<String>,
    /// Parent / referenced side. `[schema.]table.column`.
    #[serde(default)]
    pub to: Option<String>,
    /// Cardinality token (`one-to-many`, `many-to-one`, ...). Default
    /// `many-to-one` — the most common case for logical FK-less joins.
    #[serde(default = "default_cardinality")]
    pub cardinality: String,
    /// Optional human note shown in the TUI Impact view and in
    /// rendered diagrams as part of the edge label.
    #[serde(default)]
    pub note: Option<String>,
    /// Composite-relation support (reserved for V1.1). Empty in V1.
    #[serde(default)]
    pub from_columns: Vec<String>,
    #[serde(default)]
    pub to_columns: Vec<String>,
}

fn default_cardinality() -> String {
    "many-to-one".into()
}

/// Container for the persisted connection list and any global
/// logical-relation declarations.
///
/// Wire-format v2 adds a top-level `schema_version = 2` discriminant
/// for symmetry with `settings.toml`. v1 files (no discriminant) are
/// still parsed but [`ConnectionsFile::load`] returns
/// [`ConfigError::NeedsMigration`] so the CLI can prompt for
/// `narwhal migrate-config`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ConnectionsFile {
    /// Present on v2 files. v1 files omit it and migration sets it.
    /// Serialised first so the file's first line is the discriminant.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema_version: Option<u32>,
    #[serde(rename = "connection", default)]
    pub connections: Vec<ConnectionConfig>,
    /// Logical relations declared at the user level. Each entry
    /// carries its own `connection` field so a single file can
    /// describe relations across several connections.
    #[serde(rename = "logical_relation", default)]
    pub logical_relations: Vec<LogicalRelationConfig>,
}

impl Settings {
    /// Load `settings.toml` from `path`. v2 files are returned as-is;
    /// v1 files return [`ConfigError::NeedsMigration`] so the caller
    /// (typically the binary's CLI dispatcher) can suggest `narwhal
    /// migrate-config`. Missing file → [`Settings::default`].
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = std::fs::read_to_string(path)?;
        match peek_schema_version(&text)? {
            None => Err(ConfigError::NeedsMigration {
                path: path.to_path_buf(),
            }),
            Some(1) => Err(ConfigError::NeedsMigration {
                path: path.to_path_buf(),
            }),
            Some(2) => {
                let file: SettingsFile = toml::from_str(&text)?;
                Ok(file.settings)
            }
            Some(n) => Err(ConfigError::UnsupportedSchema(n)),
        }
    }

    /// Parse the v1 layout (no `schema_version`, sections at top
    /// level). Used by the migrate-config CLI; not part of the
    /// happy-path `load` because v1 files must be migrated
    /// explicitly.
    pub fn load_v1(path: &Path) -> Result<Self, ConfigError> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = std::fs::read_to_string(path)?;
        Self::load_v1_from_str(&text)
    }

    /// Parse the v1 layout from a TOML string. Public for tests +
    /// the migrate-config CLI.
    pub fn load_v1_from_str(text: &str) -> Result<Self, ConfigError> {
        // v1 had all sections at the top level with no schema_version.
        // The current Settings struct is layout-compatible because
        // the v2 additions (workspace / vault / plugins) all have
        // serde defaults — a v1 file simply leaves them empty.
        let s: Self = toml::from_str(text)?;
        Ok(s)
    }

    /// Serialise as v2 and atomically write to `path`. Adds
    /// `schema_version = 2` at the top of the file.
    pub fn save(&self, path: &Path) -> Result<(), ConfigError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let envelope = SettingsFile {
            schema_version: CURRENT_SCHEMA_VERSION,
            settings: self.clone(),
        };
        let text = toml::to_string_pretty(&envelope)?;
        atomic_write(path, &text).map_err(ConfigError::from)?;
        Ok(())
    }
}

/// `pub(crate)` thin wrapper so the migrate module can reuse the
/// scanner without copying the heuristic.
pub(crate) fn peek_schema_version_public(text: &str) -> Result<Option<u32>, ConfigError> {
    peek_schema_version(text)
}

/// Peek at the `schema_version` top-level key without doing a full
/// TOML parse. Returns `None` if the key is absent (v1 shape).
///
/// We scan the file's header lines (the region before any `[table]`)
/// for a literal `schema_version = N` assignment. Toml allows
/// top-level scalar keys before any table, and this discriminant is
/// always written as the very first key by [`SettingsFile`] /
/// [`ConnectionsFile`] serialisation. The scanner is O(header)
/// rather than O(file), so a large `keymap` table doesn't pay for
/// the version peek on every load. We deliberately accept only the
/// canonical bare-identifier-equals-integer spelling here — anything
/// fancier (quoted key, hex value) falls through to the full TOML
/// parser below as a safety net.
fn peek_schema_version(text: &str) -> Result<Option<u32>, ConfigError> {
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') {
            // Entered the first table; the discriminant must precede
            // every `[table]` block by TOML rules, so we're done.
            return Ok(None);
        }
        if let Some(rest) = line.strip_prefix("schema_version") {
            let after = rest.trim_start();
            if let Some(value) = after.strip_prefix('=') {
                let token = value.trim_end_matches([' ', '\t']).trim();
                // Strip a trailing inline comment.
                let token = token.split('#').next().unwrap_or("").trim();
                return token.parse::<u32>().map(Some).map_err(|e| {
                    ConfigError::Validation(format!("invalid schema_version `{token}`: {e}"))
                });
            }
        }
    }
    // Fallback for fancy spellings (quoted key, multi-line
    // assignments). Rare; if the cheap scan missed it, the full TOML
    // parser is the source of truth.
    #[derive(Deserialize)]
    struct Header {
        schema_version: Option<u32>,
    }
    let header: Header = toml::from_str(text)?;
    Ok(header.schema_version)
}

impl ConnectionsFile {
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = std::fs::read_to_string(path)?;
        match peek_schema_version(&text)? {
            None => Err(ConfigError::NeedsMigration {
                path: path.to_path_buf(),
            }),
            Some(1) => Err(ConfigError::NeedsMigration {
                path: path.to_path_buf(),
            }),
            Some(2) => Self::load_v2_from_str(&text),
            Some(n) => Err(ConfigError::UnsupportedSchema(n)),
        }
    }

    fn load_v2_from_str(text: &str) -> Result<Self, ConfigError> {
        let mut file: Self = toml::from_str(text)?;
        // L36 #6: expand `${env:VAR}` placeholders in every string
        // field before validation — missing variables surface as a
        // ConfigError instead of a confusing engine-level connect
        // failure later on.
        crate::interpolate::interpolate_connections(&mut file)
            .map_err(|e| ConfigError::Interpolate(e.to_string()))?;
        validate_connections(&file.connections)?;
        Ok(file)
    }

    /// Parse the v1 layout (no `schema_version`) from disk. Used by
    /// the migrate-config CLI.
    pub fn load_v1(path: &Path) -> Result<Self, ConfigError> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = std::fs::read_to_string(path)?;
        let mut file: Self = toml::from_str(&text)?;
        // Discard any stray schema_version a malformed file might carry.
        file.schema_version = None;
        validate_connections(&file.connections)?;
        Ok(file)
    }

    pub fn save(&self, path: &Path) -> Result<(), ConfigError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        // Validate before writing so corrupt configs are never persisted.
        validate_connections(&self.connections)?;
        let mut file = self.clone();
        file.schema_version = Some(CURRENT_SCHEMA_VERSION);
        let text = toml::to_string_pretty(&file)?;
        atomic_write(path, &text).map_err(ConfigError::from)?;
        Ok(())
    }

    /// Parse a TOML string directly (useful for tests). Skips
    /// `${env:…}` interpolation so test fixtures can assert against
    /// the raw placeholder text without setting up process env vars.
    /// Accepts both v1 and v2 wire formats — tests don't care.
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
