//! Shared immutable services.
//!
//! Everything wired in once at startup and read (or mutated through
//! interior `Arc<Mutex<…>>` handles) for the lifetime of the
//! application. Bundling these in one struct makes the dependency
//! surface obvious: a test fixture only has to construct an
//! `AppDeps` to mock the entire I/O boundary.
//!
//! The keymap lives here too because callers borrow it immutably on
//! the hot path; the rare mutation (settings reload) goes through
//! `AppCore::apply_settings` which already takes `&mut self`.

use std::sync::Arc;

use narwhal_config::{DynCredentialStore, VaultRegistry};
use narwhal_plugin::PluginRegistry;

use super::super::plugin_executor::PluginConnectionState;
use crate::clipboard::Clipboard;
use crate::keymap::Keymap;
use crate::registry::DriverRegistry;

/// Wiring established at startup. Cheap to clone (everything is an
/// `Arc` or owned-value handle); cheap to pass through trait
/// objects in tests.
pub struct AppDeps {
    /// Driver lookup. Owns the registered `DatabaseDriver`
    /// implementations; `:open <name>` consults it.
    pub registry: DriverRegistry,
    /// Credential store. Backed by libsecret / Windows DPAPI /
    /// macOS Keychain in production; `InMemoryStore` in tests.
    pub credentials: Arc<dyn DynCredentialStore>,
    /// Secret-vault provider registry. Empty by default;
    /// the binary swaps in a populated [`VaultRegistry`] built from
    /// `settings.vault.providers`. Cheap to clone (every provider
    /// is an `Arc<dyn VaultProvider>`).
    pub vault: Arc<VaultRegistry>,
    /// Clipboard handle. Backed by OSC52 / pbcopy / xclip in
    /// production; `InMemoryClipboard` in tests.
    pub clipboard: Arc<dyn Clipboard>,
    /// Loaded plugin scripts. Reserved built-in command slots
    /// guarantee a plugin cannot shadow `:open`, `:run`, etc.
    pub plugins: Arc<PluginRegistry>,
    /// Shared handle the plugin SQL executor reads on every
    /// `narwhal.sql_run` call. Updated whenever a session opens
    /// or closes so scripts always target the currently-active
    /// connection. Interior mutability via `Mutex` because the
    /// executor runs on a tokio worker, not the UI thread.
    pub plugin_state: Arc<std::sync::Mutex<PluginConnectionState>>,
    /// Active key map. Starts as the built-in defaults; mutated
    /// in place by `AppCore::apply_settings` whenever the user's
    /// `config.toml` supplies a `[keymap.<group>]` override.
    pub keymap: Keymap,
}
