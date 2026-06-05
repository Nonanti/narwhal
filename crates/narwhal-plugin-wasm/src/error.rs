//! Error types produced by the WASM plugin runtime.
//!
//! [`WasmError`] is the local error vocabulary; it converts cleanly to
//! [`narwhal_plugin::PluginError`] at the trait boundary so the host
//! sees a single error shape regardless of which runtime produced it.

use std::path::PathBuf;

use narwhal_plugin::PluginError;
use thiserror::Error;

use crate::capability::CapabilityParseError;

/// Errors raised while loading or executing a WASM plugin.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum WasmError {
    /// The `.wasm` file or its sibling `plugin.toml` could not be read.
    #[error("plugin file {path:?} unreadable: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// `plugin.toml` failed to parse.
    #[error("plugin manifest {path:?} invalid: {source}")]
    Manifest {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },
    /// The manifest declares a capability the host has not granted.
    #[error("plugin manifest declares capability '{capability}' which the host denies")]
    CapabilityDenied { capability: String },
    /// One of the manifest's `capabilities = [...]` entries failed
    /// to parse. Carries both the offending raw token and the
    /// parser's structured error so embedders can surface a precise
    /// message to the operator.
    #[error("plugin manifest capability token '{token}' is invalid: {source}")]
    CapabilityToken {
        token: String,
        #[source]
        source: CapabilityParseError,
    },
    /// The component's declared `api-version` is incompatible with the
    /// host. Holds the offending major number for the audit trail.
    #[error("plugin '{name}' built against api-version {plugin_major}, host supports {host_major}")]
    ApiVersion {
        name: String,
        plugin_major: u32,
        host_major: u32,
    },
    /// Wasmtime refused the component (parse error, link error, …).
    #[error("wasmtime error: {0}")]
    Wasmtime(String),
    /// The plugin's `init` export returned `err(msg)` or trapped.
    #[error("plugin '{name}' init failed: {message}")]
    Init { name: String, message: String },
    /// A plugin export trapped or exceeded the fuel budget.
    #[error("plugin '{name}' trapped: {message}")]
    Trap { name: String, message: String },
    /// State-set was called with a value that would overrun the
    /// per-plugin KV budget.
    #[error("plugin '{name}' KV budget exhausted ({used}/{budget} bytes)")]
    KvBudget {
        name: String,
        used: usize,
        budget: usize,
    },
}

impl From<WasmError> for PluginError {
    fn from(value: WasmError) -> Self {
        match value {
            // Init failures and capability denials surface to the user
            // as registration errors — same shape Lua plugins use when
            // their script fails to register.
            WasmError::CapabilityDenied { .. }
            | WasmError::CapabilityToken { .. }
            | WasmError::ApiVersion { .. }
            | WasmError::Init { .. } => Self::Handler(value.to_string()),
            // Everything else is a runtime fault.
            other => Self::Runtime(other.to_string()),
        }
    }
}

/// Convenience alias matching the rest of the workspace.
pub type WasmResult<T> = std::result::Result<T, WasmError>;
