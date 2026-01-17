//! Persistent configuration and credential storage.

#![forbid(unsafe_code)]
#![warn(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::todo,
    clippy::unimplemented,
    clippy::dbg_macro,
    clippy::print_stdout,
    clippy::print_stderr
)]

pub mod credentials;
pub mod last_used;
pub mod paths;
pub mod pgpass;
pub mod settings;
pub mod url;

pub use credentials::{CredentialError, CredentialStore, InMemoryStore, KeyringStore};
pub use last_used::{LastUsedError, LastUsedStore};
pub use paths::{ConfigPaths, PathsError};
pub use pgpass::{
    password_from_env, password_from_pgpass, resolve_password as resolve_fallback_password,
};
pub use secrecy::SecretString;
pub use settings::{
    ConfigError, ConnectionsFile, EditorSettings, KeybindingSettings, Settings, Theme,
};
pub use url::{parse as parse_url, ParsedUrl, UrlError};
