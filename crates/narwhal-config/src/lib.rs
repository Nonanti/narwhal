//! Persistent configuration and credential storage.

#![forbid(unsafe_code)]

pub mod credentials;
pub mod interpolate;
pub mod last_used;
pub mod migrate;
pub mod paths;
pub mod pgpass;
pub mod settings;
pub mod url;
pub mod vault;

pub use credentials::{
    CredentialError, CredentialStore, DynCredentialStore, InMemoryStore, KeyringStore,
    resolve_password as resolve_connection_password,
};
// `VaultProvider` is intentionally NOT re-exported here — the
// settings module already exports an enum by that name (the
// `default_provider = "none" | "hashicorp" | ...` discriminant).
// The trait lives at `narwhal_config::vault::VaultProvider` so the
// two namespaces stay distinct without renaming the enum (which
// would be a breaking change to settings v2 wire format).
pub use interpolate::{InterpolateError, interpolate, interpolate_connections};
pub use last_used::{LastUsedError, LastUsedStore};
pub use paths::{ConfigPaths, PathsError};
pub use pgpass::{
    password_from_env, password_from_pgpass, resolve_password as resolve_fallback_password,
};
pub use secrecy::SecretString;
pub use vault::{Reference, VaultError, VaultRegistry};
pub mod logical_relations;
pub use logical_relations::{
    WORKSPACE_FILE, collect_logical_relations_for, discover_workspace_root,
    read_workspace_logical_relations,
};
pub use migrate::{
    MigrateOptions, MigrateOutcome, MigrateReport, ValidateOutcome, ValidateReport,
    migrate as migrate_config, migrate_connections, migrate_settings, render_settings_v2,
    validate as validate_config,
};
pub use settings::{
    CURRENT_SCHEMA_VERSION, ConfigError, ConnectionsFile, DiagramIcons, DiagramSettings,
    EditorMode, EditorSettings, HashicorpVaultSettings, KeyPreset, KeybindingSettings,
    LogicalRelationConfig, LspSettings, MouseSelectionMode, OnePasswordVaultSettings,
    PluginSettings, RunSettings, Settings, Theme, VaultProvider, VaultProviderSettings,
    VaultSettings, WasmPluginSettings, WorkspacePersistSettings, WorkspaceSettings,
};
pub use url::{ParsedUrl, UrlError, parse as parse_url};
