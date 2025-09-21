//! Persistent configuration and credential storage.

#![forbid(unsafe_code)]

pub mod credentials;
pub mod paths;
pub mod settings;

pub use credentials::{CredentialError, CredentialStore, KeyringStore};
pub use paths::{ConfigPaths, PathsError};
pub use settings::{
    ConfigError, ConnectionsFile, EditorSettings, KeybindingSettings, Settings, Theme,
};
