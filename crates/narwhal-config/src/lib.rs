//! narwhal-config — TOML configuration and OS-keychain credential storage.

pub mod credentials;
pub mod paths;
pub mod settings;

pub use credentials::{CredentialStore, KeyringStore};
pub use paths::ConfigPaths;
pub use settings::{Settings, Theme};
