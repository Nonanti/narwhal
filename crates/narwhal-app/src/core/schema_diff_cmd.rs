//! `:schema-diff source target` handler.
//!
//! T2-T2-C of the v2.0 roadmap. Opens two saved connections
//! transiently, walks each catalogue via `list_all_tables` +
//! `describe_table`, runs the `narwhal-schema-diff` algorithm, and
//! dumps the emitted DDL into a fresh editor tab so the operator
//! can review (and execute) inside their existing workflow.
//!
//! This handler is intentionally simpler than the dedicated TUI
//! modal the brief outlines — the editor tab is a perfectly
//! reasonable "review surface" that already supports yank, save,
//! and direct execution against the active session. A full
//! tree+preview modal is a v2.1 polish task.
//!
//! Distinct from `core::diff_schema`: that one diffs **two tables
//! in the active connection**; this one diffs **two whole
//! connections**.

use std::collections::HashMap;
use std::sync::Arc;

use narwhal_core::schema::TableSchema;
use narwhal_core::{ConnectionConfig, DynConnection, TableKind};
use narwhal_schema_diff::emit::emitter_by_name;
use secrecy::ExposeSecret;
use tracing::warn;

use super::AppCore;

impl AppCore {
    pub(super) async fn schema_diff_command(
        &mut self,
        source: String,
        target: String,
        dialect: Option<String>,
        schema: Option<String>,
        table: Option<String>,
        schema_map: Vec<(String, String)>,
    ) {
        let Some(source_cfg) = self.lookup_connection_config(&source) else {
            self.ui.status.message = format!("schema-diff: source `{source}` not found");
            return;
        };
        let Some(target_cfg) = self.lookup_connection_config(&target) else {
            self.ui.status.message = format!("schema-diff: target `{target}` not found");
            return;
        };

        // Self-diff: short-circuit before we open the same connection
        // twice. The operator probably meant to type two different
        // names; surfacing the no-op explicitly beats opening a
        // blank tab.
        if source_cfg.id == target_cfg.id {
            self.ui.status.message =
                format!("schema-diff: source and target both refer to `{source}`");
            return;
        }

        // Resolve dialect *before* the (slow) introspection so a
        // typo bails immediately.
        let dialect_name = dialect.unwrap_or_else(|| source_cfg.driver.clone());
        let Some(emitter) = emitter_by_name(&dialect_name) else {
            self.ui.status.message = format!(
                "schema-diff: unknown dialect `{dialect_name}` \
                 (recognised: postgres, mysql, sqlite, mssql, generic)"
            );
            return;
        };

        self.ui.status.message = format!("schema-diff: introspecting `{source}` and `{target}`…");

        let source_tables = match self.introspect_connection(&source_cfg).await {
            Ok(t) => t,
            Err(message) => {
                self.ui.status.message = format!("schema-diff: source `{source}`: {message}");
                return;
            }
        };
        let target_tables = match self.introspect_connection(&target_cfg).await {
            Ok(t) => t,
            Err(message) => {
                self.ui.status.message = format!("schema-diff: target `{target}`: {message}");
                return;
            }
        };

        let source_filtered = apply_filters(source_tables, schema.as_deref(), table.as_deref());
        let target_filtered = apply_filters(target_tables, schema.as_deref(), table.as_deref());

        let map: HashMap<String, String> = schema_map.into_iter().collect();
        let target_mapped = apply_schema_map(target_filtered, &map);

        let diff = narwhal_schema_diff::diff(&source_filtered, &target_mapped);
        if diff.is_empty() {
            self.ui.status.message =
                format!("schema-diff: `{source}` and `{target}` are identical");
            return;
        }

        let ddl = match emitter.emit(&diff) {
            Ok(s) => s,
            Err(error) => {
                self.ui.status.message = format!("schema-diff: emit failed: {error}");
                return;
            }
        };

        // Top-of-buffer summary so the user can see what they're
        // about to execute without scrolling.
        let mut buf = String::with_capacity(ddl.len() + 128);
        buf.push_str(&format!(
            "-- schema-diff: {source}  ->  {target}  ({dialect})\n",
            dialect = emitter.name(),
        ));
        buf.push_str(&format!(
            "-- {tables} table(s) changed, {changes} total delta(s)\n\n",
            tables = diff.tables.len(),
            changes = diff.change_count(),
        ));
        buf.push_str(&ddl);

        self.new_tab().await;
        let tab = &mut self.ui.tabs[self.ui.active_tab];
        tab.editor.insert_str(&buf);
        self.ui.status.message = format!(
            "schema-diff: {} table change(s), {} delta(s) in new tab",
            diff.tables.len(),
            diff.change_count(),
        );
    }

    /// Look up a saved connection by name (exact match, then case-
    /// insensitive fallback so a typo like `Prod` still resolves
    /// to `prod`).
    fn lookup_connection_config(&self, name: &str) -> Option<ConnectionConfig> {
        self.session
            .connections
            .connections
            .iter()
            .find(|c| c.name == name)
            .or_else(|| {
                self.session
                    .connections
                    .connections
                    .iter()
                    .find(|c| c.name.eq_ignore_ascii_case(name))
            })
            .cloned()
    }

    /// Open `config`, walk its catalogue, describe every user table,
    /// close. Returns a human-readable string on failure so the
    /// status bar can surface it without dragging the
    /// `narwhal_core::Error` type into the call site.
    async fn introspect_connection(
        &self,
        config: &ConnectionConfig,
    ) -> Result<Vec<TableSchema>, String> {
        let driver = self
            .deps
            .registry
            .get(&config.driver)
            .map_err(|e| format!("driver `{}`: {e}", config.driver))?;

        // Mirror the `narwhal exec` resolver: vault → keyring →
        // pgpass / env-var fallback, all collapsed into one
        // `SecretString`. A `:schema-diff` against a vault-only
        // connection therefore "just works" provided the launcher
        // already wired the registry.
        let password = narwhal_config::resolve_connection_password(
            config,
            Some(self.deps.vault.as_ref()),
            Some(self.deps.credentials.as_ref()),
        )
        .await
        .map_err(|e| format!("credential lookup failed: {e}"))?;
        let password_str = password.as_ref().map(|s| s.expose_secret().to_owned());

        let mut conn: Box<dyn DynConnection> = driver
            .connect(config, password_str.as_deref())
            .await
            .map_err(|e| format!("connect: {e}"))?;

        let catalog = conn
            .list_all_tables()
            .await
            .map_err(|e| format!("list tables: {e}"))?;
        let mut tables = Vec::new();
        for (schema, table_list) in catalog {
            for t in table_list {
                if !matches!(t.kind, TableKind::Table) {
                    continue;
                }
                match conn.describe_table(&schema.name, &t.name).await {
                    Ok(ts) => tables.push(ts),
                    Err(error) => {
                        warn!(
                            target: "narwhal::schema-diff",
                            schema = %schema.name,
                            table = %t.name,
                            error = %error,
                            "describe_table failed; skipping",
                        );
                    }
                }
            }
        }
        let _ = conn.close().await;
        // The Arc keeps the driver alive across the await chain;
        // dropping it here makes the borrow checker's job easier
        // when this fn is inlined into a select! arm in the future.
        drop::<Arc<_>>(driver);
        Ok(tables)
    }
}

/// `--schema` / `--table` filter pass.
fn apply_filters(
    tables: Vec<TableSchema>,
    schema: Option<&str>,
    table: Option<&str>,
) -> Vec<TableSchema> {
    tables
        .into_iter()
        .filter(|t| schema.is_none_or(|s| t.table.schema == s))
        .filter(|t| table.is_none_or(|n| t.table.name == n))
        .collect()
}

/// Rewrite target-side schema names so a `--schema-map
/// prod_app=staging_app` pairs cleanly with a source that uses
/// `prod_app`. Foreign-key `referenced_schema` is rewritten too so
/// the FK comparison stays sane after the move.
fn apply_schema_map(
    mut tables: Vec<TableSchema>,
    map: &HashMap<String, String>,
) -> Vec<TableSchema> {
    if map.is_empty() {
        return tables;
    }
    for t in &mut tables {
        if let Some(new_schema) = map.get(&t.table.schema) {
            t.table.schema = new_schema.clone();
        }
        for fk in &mut t.foreign_keys {
            if let Some(ref_schema) = fk.referenced_schema.as_ref() {
                if let Some(new_schema) = map.get(ref_schema) {
                    fk.referenced_schema = Some(new_schema.clone());
                }
            }
        }
    }
    tables
}
