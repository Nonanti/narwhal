//! Workspace discovery — `.narwhal/workspace.toml`.
//!
//! A *workspace* is a directory whose `.narwhal/workspace.toml` declares
//! which subset of the user-wide `connections.toml` the MCP server may
//! expose. The intent is the same as `.envrc` for direnv or
//! `.editorconfig` for IDEs: a repo-local file the team can git-commit
//! so an agent that runs from inside the project can only reach the
//! databases the project owners say it can.
//!
//! # Discovery
//!
//! Walk up from `start_dir` until either:
//! - a directory is found that contains `.narwhal/workspace.toml`
//!   (success), or
//! - the file-system root is reached (no workspace; the MCP server then
//!   defaults to exposing every connection from `connections.toml`).
//!
//! The walk mirrors how `git` finds its top-level: cheap, deterministic,
//! and predictable from the user's `pwd`.
//!
//! # Schema
//!
//! ```toml
//! # Which connections from connections.toml are visible to the agent.
//! # Empty / omitted = all of them.
//! allowed_connections = ["staging", "test"]
//!
//! # When false, `run_query` rejects `read_only = false` and any other
//! # write-capable tool refuses to proceed. Defaults to true so a freshly
//! # committed workspace.toml that forgets the field is permissive in the
//! # workspace-less direction (less surprising on first adoption).
//! allow_writes = true
//! ```
//!
//! # Future extensions (deliberately not in v0.3)
//!
//! - Per-connection `allowed_schemas` / `allowed_tables` (table-level
//!   ACL). Today the MCP surface has no list/describe call that would
//!   benefit, so adding it now is speculative.
//! - Per-connection `allow_writes` override.
//! - Inheriting from a parent workspace via `extends = "../parent.toml"`.

use std::path::{Path, PathBuf};

use serde::Deserialize;
use thiserror::Error;

/// Default file location relative to the workspace root.
pub const WORKSPACE_FILE: &str = ".narwhal/workspace.toml";

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum WorkspaceError {
    #[error("io reading workspace file {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("parsing workspace file {path}: {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceFile {
    /// Connection name allow-list. Empty = expose everything.
    #[serde(default)]
    pub allowed_connections: Vec<String>,
    /// When `false`, every write-capable code path refuses to proceed.
    #[serde(default = "default_allow_writes")]
    pub allow_writes: bool,
    /// User-declared logical (FK-less) relations. Each entry targets
    /// a connection by name; entries for other connections are
    /// ignored by `get_diagram`. Commit this file to git so the team
    /// shares the same logical schema view.
    #[serde(rename = "logical_relation", default)]
    pub logical_relations: Vec<narwhal_config::LogicalRelationConfig>,
}

const fn default_allow_writes() -> bool {
    true
}

/// Resolved workspace, ready to be attached to a [`crate::ServerContext`].
///
/// `root` carries the directory the file was discovered in so log
/// messages can show the user which workspace took effect.
#[derive(Debug, Clone)]
pub struct Workspace {
    pub root: PathBuf,
    pub file: WorkspaceFile,
}

impl Workspace {
    /// Walk upward from `start` looking for the marker file. Returns
    /// `Ok(None)` when no marker is found — that is a legitimate state,
    /// not an error, so callers branch on `Option` instead of catching.
    pub fn discover(start: &Path) -> Result<Option<Self>, WorkspaceError> {
        let canonical_start = start.canonicalize().unwrap_or_else(|_| start.to_path_buf());
        let mut cursor: &Path = &canonical_start;
        loop {
            let candidate = cursor.join(WORKSPACE_FILE);
            if candidate.is_file() {
                let text =
                    std::fs::read_to_string(&candidate).map_err(|source| WorkspaceError::Io {
                        path: candidate.clone(),
                        source,
                    })?;
                let file: WorkspaceFile =
                    toml::from_str(&text).map_err(|source| WorkspaceError::Parse {
                        path: candidate.clone(),
                        source,
                    })?;
                return Ok(Some(Self {
                    root: cursor.to_path_buf(),
                    file,
                }));
            }
            match cursor.parent() {
                Some(parent) => cursor = parent,
                None => return Ok(None),
            }
        }
    }

    /// True when `name` is allowed by the workspace's connection
    /// allow-list. An empty list is treated as "allow everything".
    pub fn connection_allowed(&self, name: &str) -> bool {
        if self.file.allowed_connections.is_empty() {
            return true;
        }
        self.file
            .allowed_connections
            .iter()
            .any(|allowed| allowed == name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write_workspace(dir: &Path, body: &str) {
        let inner = dir.join(".narwhal");
        fs::create_dir_all(&inner).expect("mkdir .narwhal");
        fs::write(inner.join("workspace.toml"), body).expect("write");
    }

    #[test]
    fn discover_returns_none_when_no_workspace_above() {
        let dir = tempfile::tempdir().expect("tempdir");
        let nested = dir.path().join("a/b/c");
        fs::create_dir_all(&nested).expect("mkdir");
        assert!(Workspace::discover(&nested).expect("discover").is_none());
    }

    #[test]
    fn discover_finds_workspace_in_parent_directory() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_workspace(dir.path(), "allowed_connections = [\"prod\"]");
        let nested = dir.path().join("a/b/c");
        fs::create_dir_all(&nested).expect("mkdir");

        let ws = Workspace::discover(&nested)
            .expect("discover")
            .expect("workspace must be found");
        assert_eq!(ws.file.allowed_connections, vec!["prod".to_string()]);
        assert!(ws.file.allow_writes, "default must be true");
    }

    #[test]
    fn empty_allow_list_allows_anything() {
        let ws = Workspace {
            root: PathBuf::from("/"),
            file: WorkspaceFile::default(),
        };
        assert!(ws.connection_allowed("anything"));
    }

    #[test]
    fn non_empty_list_is_a_strict_allow_list() {
        let ws = Workspace {
            root: PathBuf::from("/"),
            file: WorkspaceFile {
                allowed_connections: vec!["a".into(), "b".into()],
                allow_writes: true,
                logical_relations: Vec::new(),
            },
        };
        assert!(ws.connection_allowed("a"));
        assert!(!ws.connection_allowed("c"));
    }

    #[test]
    fn allow_writes_defaults_to_true_when_field_omitted() {
        let ws: WorkspaceFile = toml::from_str("").expect("parse");
        assert!(ws.allow_writes, "absent field must default to permissive");
    }

    #[test]
    fn parse_error_surfaces_the_offending_path() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_workspace(dir.path(), "this is not toml = = =");
        let err = Workspace::discover(dir.path()).unwrap_err();
        match err {
            WorkspaceError::Parse { path, .. } => {
                assert!(path.ends_with(".narwhal/workspace.toml"));
            }
            other => panic!("expected Parse error, got {other:?}"),
        }
    }

    #[test]
    fn unknown_keys_are_rejected_so_typos_surface() {
        // `deny_unknown_fields` is on so `allow_write` (missing `s`)
        // doesn't silently get ignored.
        let dir = tempfile::tempdir().expect("tempdir");
        write_workspace(dir.path(), "allow_write = false");
        assert!(matches!(
            Workspace::discover(dir.path()),
            Err(WorkspaceError::Parse { .. })
        ));
    }
}
