//! Logical-relation resolution: convert [`LogicalRelationConfig`]
//! entries (parsed from `connections.toml` or `.narwhal/workspace.toml`)
//! into the runtime [`narwhal_domain::LogicalRelation`] type the
//! diagram model consumes.
//!
//! This module knows nothing about live `TableSchema` data; column /
//! table existence is validated downstream by
//! `narwhal_diagram::build_with_logical`, which is where the live
//! schema lives. We only handle:
//!
//! - parsing the `[schema.]table.column` token into a qualified pair,
//! - mapping the cardinality string,
//! - reading `[[logical_relation]]` blocks out of a workspace TOML.
//!
//! The workspace reader intentionally does not understand any other
//! workspace fields (`allowed_connections`, `allow_writes` — those live
//! in `narwhal-mcp`). It only opens the file, asks serde for the
//! `logical_relation` array, and lets serde drop everything else.

use std::path::{Path, PathBuf};

use serde::Deserialize;

use narwhal_domain::{Cardinality, LogicalRelation, QualifiedName};

use crate::settings::LogicalRelationConfig;

/// Path-relative location of the workspace file. Mirrors
/// `narwhal_mcp::workspace::WORKSPACE_FILE`; duplicated here so
/// `narwhal-config` does not have to depend on `narwhal-mcp` for a
/// single string.
pub const WORKSPACE_FILE: &str = ".narwhal/workspace.toml";

/// Walk up from `start_dir` looking for `.narwhal/workspace.toml`.
/// Returns the workspace **root directory** (the one containing the
/// `.narwhal` folder), or `None` on file-system root with no hit.
pub fn discover_workspace_root(start_dir: &Path) -> Option<PathBuf> {
    let mut current: Option<&Path> = Some(start_dir);
    while let Some(dir) = current {
        if dir.join(WORKSPACE_FILE).is_file() {
            return Some(dir.to_path_buf());
        }
        current = dir.parent();
    }
    None
}

/// Read `[[logical_relation]]` blocks from `<root>/.narwhal/workspace.toml`.
///
/// Returns an empty vec when the file does not exist or has no
/// `logical_relation` entries. Parse errors propagate as a string the
/// host can surface in the status bar.
pub fn read_workspace_logical_relations(
    workspace_root: &Path,
) -> Result<Vec<LogicalRelationConfig>, String> {
    let path = workspace_root.join(WORKSPACE_FILE);
    if !path.is_file() {
        return Ok(Vec::new());
    }
    let text =
        std::fs::read_to_string(&path).map_err(|e| format!("read {}: {e}", path.display()))?;

    #[derive(Deserialize, Default)]
    struct Wrapper {
        #[serde(rename = "logical_relation", default)]
        logical_relations: Vec<LogicalRelationConfig>,
    }

    let parsed: Wrapper =
        toml::from_str(&text).map_err(|e| format!("parse {}: {e}", path.display()))?;
    Ok(parsed.logical_relations)
}

/// Collect every `LogicalRelationConfig` that targets `connection_name`
/// across both the user-level `connections.toml` and a workspace file
/// (when present). Workspace entries come first so they win on
/// duplicate-key conflicts.
///
/// Returns `(relations, warnings)`. Warnings are non-fatal parse /
/// validation diagnostics the host surfaces in the status bar.
pub fn collect_logical_relations_for(
    connection_name: &str,
    connections_file: &crate::ConnectionsFile,
    workspace_root: Option<&Path>,
) -> (Vec<LogicalRelation>, Vec<String>) {
    let mut warnings = Vec::new();
    let mut configs: Vec<(LogicalRelationConfig, &'static str)> = Vec::new();

    if let Some(root) = workspace_root {
        match read_workspace_logical_relations(root) {
            Ok(items) => {
                for item in items {
                    if item.connection == connection_name {
                        configs.push((item, "workspace.toml"));
                    }
                }
            }
            Err(error) => warnings.push(format!("workspace logical_relations: {error}")),
        }
    }
    for item in &connections_file.logical_relations {
        if item.connection == connection_name {
            configs.push((item.clone(), "connections.toml"));
        }
    }

    let mut out = Vec::with_capacity(configs.len());
    for (config, source) in configs {
        match config_into_relation(&config) {
            Ok(rel) => out.push(rel),
            Err(error) => warnings.push(format!(
                "{source}: logical_relation (connection={}) dropped: {error}",
                config.connection
            )),
        }
    }
    (out, warnings)
}

/// Resolve a single config entry against the diagram model types.
fn config_into_relation(config: &LogicalRelationConfig) -> Result<LogicalRelation, String> {
    let cardinality = Cardinality::parse(&config.cardinality).ok_or_else(|| {
        format!(
            "unknown cardinality `{}` (expected `one-to-many`, `many-to-one`, ...)",
            config.cardinality
        )
    })?;

    // V1 explicitly does not implement composite logical relations.
    // The TOML keys are reserved so V1.1 can land without a wire
    // breaking change; if either is set, fail loudly so the user does
    // not silently lose half their declaration.
    if !config.from_columns.is_empty() || !config.to_columns.is_empty() {
        return Err(
            "composite logical relations (from_columns / to_columns) are reserved for v1.1; \
             use `from` / `to` for now"
                .into(),
        );
    }

    let from = config.from.as_deref().ok_or("missing `from` field")?;
    let to = config.to.as_deref().ok_or("missing `to` field")?;

    let (from_table, from_col) = parse_qualified_column(from)?;
    let (to_table, to_col) = parse_qualified_column(to)?;

    Ok(LogicalRelation {
        from: from_table,
        to: to_table,
        columns: vec![(from_col, to_col)],
        cardinality,
        note: config.note.clone(),
    })
}

/// Parse `[schema.]table.column` into `(QualifiedName, column)`.
///
/// Two dots → schema-qualified; one dot → unqualified table (schema
/// resolved to the engine default downstream by the diagram builder).
/// Zero or three+ dots fail explicitly with a friendly message.
fn parse_qualified_column(input: &str) -> Result<(QualifiedName, String), String> {
    let parts: Vec<&str> = input.split('.').collect();
    match parts.as_slice() {
        [table, column] => Ok((
            QualifiedName::new(String::new(), (*table).to_owned()),
            (*column).to_owned(),
        )),
        [schema, table, column] => Ok((
            QualifiedName::new((*schema).to_owned(), (*table).to_owned()),
            (*column).to_owned(),
        )),
        _ => Err(format!("expected `[schema.]table.column`, got `{input}`")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ConnectionsFile;

    fn cfg(conn: &str, from: Option<&str>, to: Option<&str>, card: &str) -> LogicalRelationConfig {
        LogicalRelationConfig {
            connection: conn.into(),
            from: from.map(str::to_owned),
            to: to.map(str::to_owned),
            cardinality: card.into(),
            note: None,
            from_columns: Vec::new(),
            to_columns: Vec::new(),
        }
    }

    #[test]
    fn parses_qualified_and_unqualified() {
        let (qn, col) = parse_qualified_column("events.user_id").unwrap();
        assert_eq!(qn.schema, "");
        assert_eq!(qn.name, "events");
        assert_eq!(col, "user_id");

        let (qn, col) = parse_qualified_column("public.events.user_id").unwrap();
        assert_eq!(qn.schema, "public");
        assert_eq!(qn.name, "events");
        assert_eq!(col, "user_id");
    }

    #[test]
    fn rejects_bare_table_or_too_many_parts() {
        assert!(parse_qualified_column("events").is_err());
        assert!(parse_qualified_column("a.b.c.d").is_err());
    }

    #[test]
    fn rejects_unknown_cardinality() {
        let c = cfg("p", Some("a.b"), Some("c.d"), "bogus");
        let err = config_into_relation(&c).unwrap_err();
        assert!(err.contains("unknown cardinality"));
    }

    #[test]
    fn rejects_composite_in_v1() {
        let mut c = cfg("p", None, None, "many-to-one");
        c.from_columns = vec!["a.b".into(), "a.c".into()];
        c.to_columns = vec!["d.e".into(), "d.f".into()];
        let err = config_into_relation(&c).unwrap_err();
        assert!(err.contains("composite"));
    }

    #[test]
    fn collect_filters_by_connection_name() {
        let cf = ConnectionsFile {
            schema_version: None,
            connections: Vec::new(),
            logical_relations: vec![
                cfg(
                    "prod",
                    Some("events.user_id"),
                    Some("users.id"),
                    "many-to-one",
                ),
                cfg("staging", Some("foo.bar"), Some("baz.qux"), "many-to-one"),
            ],
        };
        let (rels, warnings) = collect_logical_relations_for("prod", &cf, None);
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(rels.len(), 1);
        assert_eq!(rels[0].from.name, "events");
    }

    #[test]
    fn collect_records_warning_for_bad_config() {
        let cf = ConnectionsFile {
            schema_version: None,
            connections: Vec::new(),
            logical_relations: vec![cfg("prod", Some("a.b"), Some("c.d"), "bogus")],
        };
        let (rels, warnings) = collect_logical_relations_for("prod", &cf, None);
        assert!(rels.is_empty());
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("unknown cardinality"));
    }
}
