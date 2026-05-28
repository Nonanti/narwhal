//! Schema dump + explain + result export extracted from `core.rs` (L21).
//!
//! Three command handlers that don't really fit anywhere else:
//! - `:dump-schema {all|current|<name>}` produces DDL into the editor
//! buffer (offloaded to the meta channel for the `all` target).
//! - `:explain` rewrites the statement under the cursor with
//! `EXPLAIN (FORMAT JSON)` and dispatches it through `:run`.
//! - `:export csv|json|insert <path>` flushes the *visible* rows of
//! the active result to disk.
use narwhal_core::Row;

use super::{AppCore, ResultState};
use crate::commands::{DumpTarget, ExportArgs};
use crate::explain::wrap_explain;
use crate::export::{ExportFormat, ExportOptions, MarkdownOptions, export_rows};
use crate::meta::{MetaRequest, MetaUpdate};
use crate::run::RunMode;

impl AppCore {
    pub(super) async fn dump_schema(&mut self, target: DumpTarget) {
        let Some(_) = self.session.active.as_ref() else {
            self.ui.status.message = "no active connection".into();
            return;
        };

        match target {
            DumpTarget::All => {
                // Offload to the meta channel so the UI stays
                // responsive during long-running dump_schema all.
                self.dispatch_meta(MetaRequest::DumpSchemaAll {
                    tab_id: self.ui.tabs[self.ui.active_tab].id(),
                })
                .await;
                self.ui.status.message = "dump-schema: fetching DDL for all tables…".into();
            }
            DumpTarget::Current | DumpTarget::Named(_) => {
                // Current/Named targets fetch a single table's DDL;
                // the blocking call is brief enough that the
                // block_in_place overhead is negligible.
                self.dump_schema_single(target).await;
            }
        }
    }

    /// Fetch DDL for a single named or current table (synchronous path).
    async fn dump_schema_single(&mut self, target: DumpTarget) {
        let Some(session) = self.session.active.as_ref() else {
            self.ui.status.message = "no active connection".into();
            return;
        };
        let dialect = session.dialect();
        let pool = session.pool.clone();
        let schemas: Vec<(String, String)> = session
            .schemas
            .iter()
            .flat_map(|(schema, tables)| {
                tables
                    .iter()
                    .map(move |t| (schema.name.clone(), t.name.clone()))
            })
            .collect();

        let names: Vec<(String, String)> = match target {
            DumpTarget::Current => {
                if let ResultState::TableDetail { schema, .. } =
                    self.ui.tabs[self.ui.active_tab].results.active_state()
                {
                    vec![(schema.table.schema.clone(), schema.table.name.clone())]
                } else {
                    self.ui.status.message =
                        "dump-schema: select a table in the sidebar or pass a name".into();
                    return;
                }
            }
            DumpTarget::Named(ref name) => {
                if let Some(pair) = schemas.iter().find(|(_, t)| t == name).cloned() {
                    vec![pair]
                } else {
                    self.ui.status.message = format!("dump-schema: table not found: {name}");
                    return;
                }
            }
            DumpTarget::All => unreachable!("handled by dump_schema"),
        };

        if names.is_empty() {
            self.ui.status.message = "dump-schema: nothing to dump".into();
            return;
        }

        // Sprint 9 (H7): hand the describe_table loop to the meta
        // worker so the UI stays responsive. The result lands as
        // `MetaUpdate::DumpSchemaReady` and re-uses the same handler
        // already in place for `:dump-schema all`. We tag with the
        // current tab id so a tab switch during the dump still routes
        // the DDL to the originating tab (C5 invariant).
        let tab_id = self.ui.tabs[self.ui.active_tab].id();
        let meta_tx = self.process.meta_tx.clone();
        self.ui.status.message = format!("dumping {} table(s)…", names.len());
        let dialect_copy = dialect;
        tokio::spawn(async move {
            let collected: std::result::Result<Vec<_>, narwhal_core::Error> = async {
                let mut conn = pool
                    .acquire()
                    .await
                    .map_err(|e| narwhal_core::Error::Connection(e.to_string()))?;
                let mut out = Vec::with_capacity(names.len());
                for (schema, table) in names {
                    out.push(conn.describe_table(&schema, &table).await?);
                }
                Ok(out)
            }
            .await;
            let update = match collected {
                Ok(tables) => MetaUpdate::DumpSchemaReady { tab_id, tables },
                Err(error) => MetaUpdate::MetaFailed {
                    message: format!("dump-schema failed: {error}"),
                },
            };
            let _ = meta_tx.send(update).await;
            // dialect is unused on this background path; the DDL is
            // rendered in the meta-update handler which has its own
            // dialect from the active session. Suppress dead-code.
            let _ = dialect_copy;
        });
    }

    pub(super) async fn dispatch_explain(&mut self) {
        let Some(session) = self.session.active.as_ref() else {
            self.ui.status.message = "no active connection".into();
            return;
        };
        if session.driver.name() != "postgres" {
            self.ui.status.message = "explain is only supported on postgres for now".into();
            return;
        }
        let Some(sql) = crate::statements::statement_at_cursor(
            &self.ui.tabs[self.ui.active_tab].editor,
            session.dialect(),
        ) else {
            self.ui.status.message = "no statement under cursor".into();
            return;
        };
        let trimmed = sql.trim().trim_end_matches(';').trim().to_owned();
        if trimmed.is_empty() {
            self.ui.status.message = "no statement under cursor".into();
            return;
        }
        self.dispatch_batch(vec![wrap_explain(&trimmed)], RunMode::Execute)
            .await;
        self.ui.status.message = "explaining…".into();
    }

    pub(super) async fn export_results(&mut self, format: &str, path: &str, args: ExportArgs) {
        let Some(format) = ExportFormat::from_token(format) else {
            self.ui.status.message = format!(
                "unknown export format: {format} (csv|json|tsv|table|insert|parquet|markdown)"
            );
            return;
        };
        // reject flag/format mismatches up-front — the
        // dispatch layer surfaces these as a status message so the
        // user fixes the typo before a partial file is created.
        if args.compression.is_some() && format != ExportFormat::Parquet {
            self.ui.status.message = "--compression only applies to :export parquet".into();
            return;
        }
        if args.no_truncate && format != ExportFormat::Markdown {
            self.ui.status.message = "--no-truncate only applies to :export markdown".into();
            return;
        }
        let mut options = ExportOptions::default();
        if let Some(codec) = args.compression {
            options.parquet_compression = codec;
        }
        if args.no_truncate {
            options.markdown = MarkdownOptions { row_limit: None };
        }
        let (columns, rows, source_table) =
            match self.ui.tabs[self.ui.active_tab].results.active_state() {
                ResultState::Rows {
                    columns,
                    rows,
                    source_table,
                    ..
                } => (columns.clone(), rows.clone(), source_table.clone()),
                ResultState::Running { columns, rows, .. } if !columns.is_empty() => {
                    (columns.clone(), rows.clone(), None)
                }
                _ => {
                    self.ui.status.message = "no tabular result to export".into();
                    return;
                }
            };

        // Respect active filter/sort: export only the visible rows.
        let visible_indices = self.ui.tabs[self.ui.active_tab]
            .results
            .active()
            .visible_rows(&columns, &rows);
        let visible_rows: Vec<Row> = visible_indices.iter().map(|&i| rows[i].clone()).collect();

        let path_buf = std::path::PathBuf::from(path);
        match export_rows(
            &columns,
            &visible_rows,
            format,
            &path_buf,
            source_table.as_ref(),
            &options,
        ) {
            Ok(()) => {
                self.ui.status.message = format!(
                    "exported {} rows to {} ({})",
                    visible_rows.len(),
                    path_buf.display(),
                    format.default_extension()
                );
            }
            Err(error) => {
                self.ui.status.message = format!("export failed: {error}");
            }
        }
    }
}
