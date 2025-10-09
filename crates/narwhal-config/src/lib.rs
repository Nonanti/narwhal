//! Persistent configuration and credential storage.

#![forbid(unsafe_code)]

pub mod credentials;
pub mod paths;
pub mod settings;
pub mod url;

pub use credentials::{CredentialError, CredentialStore, InMemoryStore, KeyringStore};
pub use paths::{ConfigPaths, PathsError};
pub use settings::{
    ConfigError, ConnectionsFile, EditorSettings, KeybindingSettings, Settings, Theme,
};
pub use url::{parse as parse_url, ParsedUrl, UrlError};
