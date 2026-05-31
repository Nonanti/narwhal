//! Persistent configuration and credential storage.

#![forbid(unsafe_code)]

pub mod credentials;
pub mod interpolate;
pub mod last_used;
pub mod paths;
pub mod pgpass;
pub mod settings;
pub mod url;

pub use credentials::{CredentialError, CredentialStore, InMemoryStore, KeyringStore};
pub use interpolate::{interpolate, interpolate_connections, InterpolateError};
pub use last_used::{LastUsedError, LastUsedStore};
pub use paths::{ConfigPaths, PathsError};
pub use pgpass::{
    password_from_env, password_from_pgpass, resolve_password as resolve_fallback_password,
};
pub use secrecy::SecretString;
pub mod logical_relations;
pub use logical_relations::{
    collect_logical_relations_for, discover_workspace_root, read_workspace_logical_relations,
    WORKSPACE_FILE,
};
pub use settings::{
    ConfigError, ConnectionsFile, DiagramIcons, DiagramSettings, EditorSettings,
    KeybindingSettings, LogicalRelationConfig, Settings, Theme,
};
pub use url::{parse as parse_url, ParsedUrl, UrlError};
