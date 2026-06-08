//! Capability vocabulary the WASM plugin runtime understands.
//!
//! ## v2.0 model
//!
//! A *capability* is an explicit authorisation token a plugin
//! declares in its `plugin.toml` manifest. Tokens are
//! **argument-carrying**: `FsRead("/etc")` is meaningfully different
//! from `FsRead("/home/me")`. The host's [`Grants`] structure carries
//! a *granted* set; the manifest's [`CapabilitySet`] carries a
//! *requested* set; the runtime intersects the two at load time and
//! enforces every host-function entry point against the intersection.
//!
//! ### Tokens
//!
//! | Manifest string                 | Runtime variant              |
//! | ------------------------------- | ---------------------------- |
//! | `state`                         | [`Capability::State`]        |
//! | `cmd`                           | [`Capability::Cmd`]          |
//! | `cmd.invoke:<name>`             | [`Capability::CmdInvoke`]    |
//! | `fs.read:<path-prefix>`         | [`Capability::FsRead`]       |
//! | `fs.write:<path-prefix>`        | [`Capability::FsWrite`]      |
//! | `net.connect:<host>[:<port>]`   | [`Capability::NetConnect`]   |
//! | `env.read:<VAR>`                | [`Capability::EnvRead`]      |
//!
//! Path prefixes are matched **lexically** after `..` rejection —
//! see [`PathScope::contains`] for the exact rule. The runtime never
//! calls [`std::fs::canonicalize`] on a plugin-supplied path because
//! the syscall is racy and would leak host directory structure
//! through error messages.
//!
//! ### Backwards compatibility
//!
//! Pre-v2.0 manifests shipped bare unit-style tokens (`fs-read`,
//! `net`, `env`, `fs-write`). To keep already-on-disk manifests
//! loading, the parser still accepts those forms and maps them to
//! the widest-possible scope (`fs.read:/`, `net.connect:*`,
//! `env.read:*`). New manifests should prefer the explicit form.

mod parser;
mod scope;
mod set;

pub use parser::CapabilityParseError;
pub use scope::{EnvVar, HostPort, PathScope};
pub use set::{CapabilityKind, CapabilitySet, Grants};

use serde::{Deserialize, Serialize};

/// An individual capability token after parsing.
///
/// Tokens are sortable so a [`CapabilitySet`] iterates deterministically
/// — log messages and audit records are stable across runs.
///
/// `#[non_exhaustive]` so new variants land non-breaking. Match
/// against [`Capability::kind`] for forward-compatible code.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
#[non_exhaustive]
pub enum Capability {
    /// Per-plugin KV store access (`host.state-get` / `host.state-set`).
    State,
    /// Generic `host.cmd` dispatch (any command). Mostly retained
    /// for backwards compatibility; new manifests should prefer
    /// [`Capability::CmdInvoke`] with an explicit allow-list.
    Cmd,
    /// Permission to invoke a specific narwhal `:` command by name.
    CmdInvoke(String),
    /// Read access to a filesystem prefix.
    FsRead(PathScope),
    /// Write access to a filesystem prefix.
    FsWrite(PathScope),
    /// Outbound TCP connect permission for a host/port pair.
    NetConnect(HostPort),
    /// Environment variable read access.
    EnvRead(EnvVar),
}

impl Capability {
    /// Erase the argument and return the variant kind. Used by
    /// [`CapabilitySet::has_kind`] and the audit log so callers can
    /// reason about capability *categories* without naming every
    /// variant.
    #[must_use]
    pub const fn kind(&self) -> CapabilityKind {
        match self {
            Self::State => CapabilityKind::State,
            Self::Cmd => CapabilityKind::Cmd,
            Self::CmdInvoke(_) => CapabilityKind::CmdInvoke,
            Self::FsRead(_) => CapabilityKind::FsRead,
            Self::FsWrite(_) => CapabilityKind::FsWrite,
            Self::NetConnect(_) => CapabilityKind::NetConnect,
            Self::EnvRead(_) => CapabilityKind::EnvRead,
        }
    }

    /// Re-serialise to the canonical manifest string form. Round-trips
    /// through [`Capability::parse`].
    #[must_use]
    pub fn to_token(&self) -> String {
        match self {
            Self::State => "state".to_owned(),
            Self::Cmd => "cmd".to_owned(),
            Self::CmdInvoke(name) => format!("cmd.invoke:{name}"),
            Self::FsRead(scope) => format!("fs.read:{}", scope.as_str()),
            Self::FsWrite(scope) => format!("fs.write:{}", scope.as_str()),
            Self::NetConnect(hp) => format!("net.connect:{}", hp.as_str()),
            Self::EnvRead(var) => format!("env.read:{}", var.as_str()),
        }
    }

    /// Parse a manifest-form token. Accepts the legacy unit forms
    /// (`fs-read` etc.) for forward-compat with manifests.
    pub fn parse(token: &str) -> Result<Self, CapabilityParseError> {
        parser::parse(token)
    }
}

impl std::fmt::Display for Capability {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.to_token())
    }
}

// Serde round-trip: derived `(De)Serialize` would emit JSON-object
// tagged values which break the TOML-string form documented in the
// SDK. Routing through the string token form keeps the manifest
// schema stable.
impl TryFrom<String> for Capability {
    type Error = CapabilityParseError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(&value)
    }
}

impl From<Capability> for String {
    fn from(value: Capability) -> Self {
        value.to_token()
    }
}
