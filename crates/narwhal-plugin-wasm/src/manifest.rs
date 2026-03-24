//! Plugin manifest schema.
//!
//! Every WASM plugin ships with a `plugin.toml` file sitting next to
//! the `.wasm` component. The host loads the manifest first so it can
//! reject the plugin *before* spending any wasmtime instantiation
//! cycles on it — capability mismatches and api-version skew are both
//! cheap to detect in plain TOML.
//!
//! ## Schema (v0.1)
//!
//! ```toml
//! # Required: identity and ABI contract.
//! name        = "my-plugin"
//! version     = "0.1.0"
//! api-version = 1                # narwhal:plugin major version
//!
//! # Optional: where the .wasm sits relative to this file. Defaults to
//! # "<name>.wasm" in the same directory.
//! component   = "my_plugin.wasm"
//!
//! # Optional: short user-facing line for `:help`.
//! description = "Greets connections on open"
//!
//! # Capabilities the plugin needs. Each must be granted by the host
//! # `[plugins.wasm]` settings or the plugin is refused at load time.
//! # Defaults to the empty list (event-only plugins).
//! capabilities = ["state", "cmd"]
//!
//! # `:` commands the plugin handles. Must not shadow a built-in name.
//! [[commands]]
//! name        = "say-hi"
//! description = "Send a greeting log line"
//! ```

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use narwhal_plugin::CommandDescriptor;

use crate::capability::{Capability, CapabilitySet};
use crate::error::{WasmError, WasmResult};

/// Major API version the host implements. Bump only on
/// breaking-change releases of `wit/world.wit`.
pub const HOST_API_MAJOR: u32 = 1;

/// Parsed, validated `plugin.toml`.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Manifest {
    /// Stable plugin name. Used as the namespace for the host KV
    /// store and as the [`narwhal_plugin::Plugin::name`] return value.
    pub name: String,
    /// Plugin-author-chosen semver string. Not parsed; only logged.
    pub version: String,
    /// `narwhal:plugin` major version the component was built
    /// against. Must match [`HOST_API_MAJOR`].
    pub api_version: u32,
    /// Resolved absolute path to the `.wasm` component.
    pub component_path: PathBuf,
    /// One-line description for `:help`.
    pub description: String,
    /// Capability set the plugin declared. Already validated against
    /// the active host policy by [`Manifest::from_toml_str`] when a
    /// policy is provided.
    pub capabilities: CapabilitySet,
    /// Commands the plugin exposes. Stored as
    /// [`narwhal_plugin::CommandDescriptor`] so the surrounding
    /// `PluginRegistry::register` call needs no translation.
    pub commands: Vec<CommandDescriptor>,
}

impl Manifest {
    /// Read and validate a manifest from disk. `manifest_path` should
    /// point at the `plugin.toml`; the sibling `.wasm` is resolved
    /// relative to it.
    pub fn load(manifest_path: &Path) -> WasmResult<Self> {
        let text = std::fs::read_to_string(manifest_path).map_err(|source| WasmError::Io {
            path: manifest_path.to_path_buf(),
            source,
        })?;
        Self::from_toml_str(&text, manifest_path)
    }

    /// Parse a manifest from a TOML string, resolving the `.wasm`
    /// path relative to `manifest_path` (which need not exist on disk
    /// — handy for tests).
    pub fn from_toml_str(text: &str, manifest_path: &Path) -> WasmResult<Self> {
        let raw: RawManifest = toml::from_str(text).map_err(|source| WasmError::Manifest {
            path: manifest_path.to_path_buf(),
            source,
        })?;
        raw.into_manifest(manifest_path)
    }

    /// Validate the manifest's declared `api_version` against
    /// [`HOST_API_MAJOR`]. Called automatically during
    /// [`crate::Runtime::load`]; exposed so external loaders can
    /// run the same check without booting wasmtime.
    pub fn check_api_version(&self) -> WasmResult<()> {
        if self.api_version != HOST_API_MAJOR {
            return Err(WasmError::ApiVersion {
                name: self.name.clone(),
                plugin_major: self.api_version,
                host_major: HOST_API_MAJOR,
            });
        }
        Ok(())
    }
}

/// On-disk shape. Kept private so consumers depend on the validated
/// [`Manifest`] shape rather than the raw TOML projection. Fields
/// match the documented schema above; renames stay manual rather
/// than relying on `serde(rename_all)` so future field additions
/// don't accidentally inherit the rename.
#[derive(Debug, Deserialize, Serialize)]
struct RawManifest {
    name: String,
    version: String,
    #[serde(rename = "api-version")]
    api_version: u32,
    #[serde(default)]
    component: Option<String>,
    #[serde(default)]
    description: String,
    /// Raw capability tokens — parsed via
    /// [`Capability::parse`] inside [`RawManifest::into_manifest`].
    /// Storing as `Vec<String>` (rather than
    /// `Vec<Capability>`) keeps the manifest's own deserialiser
    /// independent of the capability-parser's error vocabulary —
    /// a parse failure becomes a single, well-located
    /// [`WasmError::CapabilityToken`] instead of a generic serde
    /// trace.
    #[serde(default)]
    capabilities: Vec<String>,
    #[serde(default)]
    commands: Vec<RawCommand>,
}

#[derive(Debug, Deserialize, Serialize)]
struct RawCommand {
    name: String,
    #[serde(default)]
    description: String,
}

impl RawManifest {
    fn into_manifest(self, manifest_path: &Path) -> WasmResult<Manifest> {
        let component_rel = self
            .component
            .unwrap_or_else(|| format!("{}.wasm", self.name));
        let component_path = manifest_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(component_rel);

        // Parse every capability token here so a typo produces a
        // precise [`WasmError::CapabilityToken`] tied to the
        // offending string, not a generic serde error.
        let mut parsed_caps = Vec::with_capacity(self.capabilities.len());
        for raw in &self.capabilities {
            parsed_caps.push(Capability::parse(raw).map_err(|source| {
                WasmError::CapabilityToken {
                    token: raw.clone(),
                    source,
                }
            })?);
        }
        let capabilities = CapabilitySet::from_caps(parsed_caps);
        let commands = self
            .commands
            .into_iter()
            .map(|c| CommandDescriptor {
                name: c.name,
                description: c.description,
            })
            .collect();

        Ok(Manifest {
            name: self.name,
            version: self.version,
            api_version: self.api_version,
            component_path,
            description: self.description,
            capabilities,
            commands,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
        name = "hello"
        version = "0.1.0"
        api-version = 1
        description = "greeter"
        capabilities = ["state", "cmd"]

        [[commands]]
        name = "hi"
        description = "say hi"

        [[commands]]
        name = "bye"
    "#;

    #[test]
    fn parses_full_manifest() {
        let path = Path::new("/tmp/test/plugin.toml");
        let m = Manifest::from_toml_str(SAMPLE, path).unwrap();
        assert_eq!(m.name, "hello");
        assert_eq!(m.version, "0.1.0");
        assert_eq!(m.api_version, 1);
        assert_eq!(m.description, "greeter");
        assert_eq!(m.component_path, Path::new("/tmp/test/hello.wasm"));
        assert!(m.capabilities.contains(&Capability::State));
        assert!(m.capabilities.contains(&Capability::Cmd));
        assert_eq!(m.commands.len(), 2);
        assert_eq!(m.commands[0].name, "hi");
        assert_eq!(m.commands[1].name, "bye");
        assert_eq!(m.commands[1].description, "");
    }

    #[test]
    fn explicit_component_path_overrides_default() {
        let toml = r#"
            name = "x"
            version = "0"
            api-version = 1
            component = "build/x_release.wasm"
        "#;
        let m =
            Manifest::from_toml_str(toml, Path::new("/etc/narwhal/plugins/x/plugin.toml")).unwrap();
        assert_eq!(
            m.component_path,
            Path::new("/etc/narwhal/plugins/x/build/x_release.wasm")
        );
    }

    #[test]
    fn missing_required_field_is_rejected() {
        let toml = r#"name = "x""#;
        let err = Manifest::from_toml_str(toml, Path::new("/tmp/p.toml")).unwrap_err();
        assert!(matches!(err, WasmError::Manifest { .. }));
    }

    #[test]
    fn unknown_capability_token_is_rejected() {
        let toml = r#"
            name = "x"
            version = "0"
            api-version = 1
            capabilities = ["not-a-real-cap"]
        "#;
        let err = Manifest::from_toml_str(toml, Path::new("/tmp/p.toml")).unwrap_err();
        match err {
            WasmError::CapabilityToken { token, .. } => assert_eq!(token, "not-a-real-cap"),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn argument_carrying_tokens_round_trip() {
        let toml = r#"
            name = "x"
            version = "0.1.0"
            api-version = 1
            capabilities = [
                "fs.read:/etc",
                "net.connect:api.test:443",
                "env.read:HOME",
                "cmd.invoke:run",
            ]
        "#;
        let m = Manifest::from_toml_str(toml, Path::new("/tmp/p.toml")).unwrap();
        assert_eq!(m.capabilities.len(), 4);
    }

    #[test]
    fn path_traversal_in_capability_is_rejected() {
        use crate::capability::CapabilityParseError;
        let toml = r#"
            name = "x"
            version = "0"
            api-version = 1
            capabilities = ["fs.read:/etc/../home"]
        "#;
        let err = Manifest::from_toml_str(toml, Path::new("/tmp/p.toml")).unwrap_err();
        match err {
            WasmError::CapabilityToken { source, .. } => {
                assert!(matches!(source, CapabilityParseError::PathTraversal(_)));
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn api_version_mismatch_is_caught() {
        let toml = r#"
            name = "x"
            version = "0"
            api-version = 99
        "#;
        let m = Manifest::from_toml_str(toml, Path::new("/tmp/p.toml")).unwrap();
        let err = m.check_api_version().unwrap_err();
        match err {
            WasmError::ApiVersion {
                name,
                plugin_major,
                host_major,
            } => {
                assert_eq!(name, "x");
                assert_eq!(plugin_major, 99);
                assert_eq!(host_major, HOST_API_MAJOR);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }
}
