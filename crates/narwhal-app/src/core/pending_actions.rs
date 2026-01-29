//! Result-pane mutation pipeline (L36).
//!
//! The interactive vocabulary for the row CRUD + pending changes
//! feature lives here:
//!
//! - `o` queues an `INSERT` and opens an inline editor over the first
//!   editable column so the user can populate values.
//! - `O` clones the focused row into a new `INSERT` (sans PK) ready to
//!   edit.
//! - `d` queues a `DELETE` on the focused row using the row snapshot
//!   for optimistic-concurrency safety.
//! - Cell edit (the `e` path) re-routes through here as well: the
//!   commit no longer touches the database, it just pushes a queued
//!   `Update`.
//! - `Ctrl-S` flushes the queue inside a single transaction.
//! - `Ctrl-X` discards the queue without contacting the database.
//! - `Ctrl-P` toggles the preview modal that lists the generated SQL.

use std::collections::BTreeMap;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use narwhal_core::{Column, Row, Value};
use tokio::runtime::Handle;

use super::state::PendingPreviewState;
use super::{AppCore, ResultState};
use crate::pending::{
    CompiledMutation, ExpectedRows, PendingChanges, PendingMutation, TableId,
};
use crate::run::RunTarget;

/// Tuple returned by [`AppCore::snapshot_focused_row`]: target table,
/// schema-snapshot columns, the row at queue time, the column order,
/// and the PK column → value map. Pulled into a type alias so the
/// helper's signature reads cleanly.
type FocusedRowSnapshot = (
    TableId,
    Vec<Column>,
    Row,
    Vec<String>,
    BTreeMap<String, Value>,
);

impl AppCore {
    /// Guard: every row-CRUD entry point bails early when the active
    /// driver does not expose row-level DML. Today only `ClickHouse`
    /// trips this; the user gets a single-line explanation rather
    /// than a generic engine error after the commit.
    fn dml_supported(&mut self) -> bool {
        let Some(session) = self.session.as_ref() else {
            self.status.message = "no active connection".into();
            return false;
        };
        if !session.capabilities.row_level_dml {
            self.status.message = format!(
                "{}: driver does not support row-level DML; use the SQL editor with engine-specific syntax",
                session.config.driver,
            );
            return false;
        }
        true
    }

    /// Snapshot the row currently focused in the result pane along with
    /// the table identity and PK column values. Returns `None` when
    /// the active result is not editable (no `RowSource`, no PK,
    /// nothing selected, ...) and leaves a status hint on the bar.
    fn snapshot_focused_row(&mut self) -> Option<FocusedRowSnapshot> {
        let tab = &self.tabs[self.active_tab];
        let (columns, rows, source) = if let ResultState::Rows {
            columns,
            rows,
            source: Some(source),
            ..
        } = tab.results.active_state()
        {
            (columns.clone(), rows.clone(), source.clone())
        } else {
            self.status.message = "no editable row here".into();
            return None;
        };
        if source.columns.iter().all(|c| !c.primary_key) {
            self.status.message = format!(
                "{}: no primary key — deletes and edits are disabled",
                source.table
            );
            return None;
        }
        let row_idx = self.row_index_for_focus();
        let row = rows.get(row_idx).cloned()?;
        let column_order: Vec<String> = columns.iter().map(|c| c.name.clone()).collect();
        let mut pk_values = BTreeMap::new();
        for pk in source.columns.iter().filter(|c| c.primary_key) {
            if let Some(idx) = column_order.iter().position(|c| c == &pk.name) {
                if let Some(v) = row.0.get(idx).cloned() {
                    pk_values.insert(pk.name.clone(), v);
                }
            }
        }
        Some((
            TableId::new(source.schema.clone(), source.table.clone()),
            source.columns,
            row,
            column_order,
            pk_values,
        ))
    }

    /// Local cousin of `selected_original_row` (defined on the
    /// results-actions impl): returns the original row index of the
    /// focused row, falling back to `0` when `visible_indices` is
    /// empty (no render has populated it yet).
    fn row_index_for_focus(&self) -> usize {
        let tab = &self.tabs[self.active_tab];
        let Some(vis_selected) = tab.results.active().selected() else {
            return 0;
        };
        tab.results
            .active()
            .visible_indices
            .get(vis_selected)
            .copied()
            .unwrap_or(vis_selected)
    }

    /// `o` — queue an `INSERT` whose values map is empty so the engine
    /// falls back to column defaults. The user typically then steps
    /// across the staged row in the grid and uses `e` (cell edit) to
    /// populate fields before committing.
    pub(super) fn append_row(&mut self) {
        if !self.dml_supported() {
            return;
        }
        let tab = &self.tabs[self.active_tab];
        let source = if let ResultState::Rows {
            source: Some(s), ..
        } = tab.results.active_state()
        {
            s.clone()
        } else {
            self.status.message = "open a table preview before appending rows".into();
            return;
        };
        let target = TableId::new(source.schema.clone(), source.table.clone());
        self.tabs[self.active_tab]
            .pending
            .push(PendingMutation::Insert {
                target: target.clone(),
                columns: source.columns.clone(),
                values: BTreeMap::new(),
            });
        self.status.message = format!(
            "queued INSERT on {} · edit then Ctrl-S to commit ({} pending)",
            target.display(),
            self.tabs[self.active_tab].pending.len(),
        );
    }

    /// `O` — duplicate the focused row as a fresh `INSERT`. Every
    /// non-PK column is copied; PK columns are skipped (PK is typically
    /// auto-generated; leave it to engine defaults).
    pub(super) fn duplicate_row(&mut self) {
        if !self.dml_supported() {
            return;
        }
        let Some((target, columns, row, column_order, _pk)) = self.snapshot_focused_row() else {
            return;
        };
        let mut values: BTreeMap<String, Value> = BTreeMap::new();
        for col in &columns {
            if col.primary_key {
                continue;
            }
            if let Some(idx) = column_order.iter().position(|c| c == &col.name) {
                if let Some(v) = row.0.get(idx).cloned() {
                    values.insert(col.name.clone(), v);
                }
            }
        }
        let count_before = self.tabs[self.active_tab].pending.len();
        self.tabs[self.active_tab]
            .pending
            .push(PendingMutation::Insert {
                target: target.clone(),
                columns,
                values,
            });
        self.status.message = format!(
            "duplicated row on {} · {} pending",
            target.display(),
            count_before + 1,
        );
    }

    /// `d` — queue a `DELETE` on the focused row.
    pub(super) fn delete_row(&mut self) {
        if !self.dml_supported() {
            return;
        }
        let Some((target, columns, row, column_order, pk_values)) = self.snapshot_focused_row()
        else {
            return;
        };
        if pk_values.values().any(Value::is_null) {
            self.status.message = format!(
                "{}: PK column is NULL on this row — cannot DELETE safely",
                target.display()
            );
            return;
        }
        let count_before = self.tabs[self.active_tab].pending.len();
        self.tabs[self.active_tab]
            .pending
            .push(PendingMutation::Delete {
                target: target.clone(),
                columns,
                pk_values,
                snapshot: row,
                column_order,
            });
        self.status.message = format!(
            "queued DELETE on {} · {} pending",
            target.display(),
            count_before + 1,
        );
    }

    /// Bridge from the cell editor to the pending queue. Called when
    /// the user hits Enter inside the inline editor. Replaces the
    /// previous immediate-execute behaviour (L21) with a pending-aware
    /// path: the change goes into the queue and the in-memory grid is
    /// patched so the user sees the new value while it is still
    /// uncommitted.
    pub(super) fn queue_cell_edit_commit(&mut self) {
        if !self.dml_supported() {
            // Drop the in-flight edit so the modal closes — leaving
            // it open would suggest the change will land.
            self.tabs[self.active_tab].editing = None;
            self.tabs[self.active_tab].results.active_mut().edit = None;
            return;
        }
        let Some(edit) = self.tabs[self.active_tab].editing.clone() else {
            return;
        };
        let (columns, rows, source) = if let ResultState::Rows {
            columns,
            rows,
            source: Some(source),
            ..
        } = self.tabs[self.active_tab].results.active_state()
        {
            (columns.clone(), rows.clone(), source.clone())
        } else {
            self.set_edit_error("result is no longer editable".into());
            return;
        };
        let Some(row) = rows.get(edit.row_index).cloned() else {
            self.set_edit_error("row went away under the editor".into());
            return;
        };
        let hint = columns
            .iter()
            .find(|c| c.name == edit.column_name)
            .map(|c| c.data_type.as_str());
        let new_value = crate::cell_edit::parse_input_typed(&edit.buffer, hint);
        let column_order: Vec<String> = columns.iter().map(|c| c.name.clone()).collect();
        let mut pk_values: BTreeMap<String, Value> = BTreeMap::new();
        let pk_columns: Vec<&narwhal_core::Column> =
            source.columns.iter().filter(|c| c.primary_key).collect();
        if pk_columns.is_empty() {
            self.set_edit_error(format!(
                "{}: no primary key, cell edits are disabled",
                source.table
            ));
            return;
        }
        for pk in &pk_columns {
            let Some(idx) = column_order.iter().position(|c| c == &pk.name) else {
                self.set_edit_error(format!(
                    "primary key column '{}' is not present in the result set",
                    pk.name
                ));
                return;
            };
            let value = row.0.get(idx).cloned().unwrap_or(Value::Null);
            if value.is_null() {
                self.set_edit_error(format!(
                    "primary key column '{}' is NULL in this row; refusing to UPDATE",
                    pk.name
                ));
                return;
            }
            pk_values.insert(pk.name.clone(), value);
        }
        let old_value = row
            .0
            .get(edit.column_index)
            .cloned()
            .unwrap_or(Value::Null);
        let target = TableId::new(source.schema.clone(), source.table.clone());
        self.tabs[self.active_tab]
            .pending
            .push(PendingMutation::Update {
                target: target.clone(),
                columns: source.columns.clone(),
                column_name: edit.column_name.clone(),
                old_value,
                new_value: new_value.clone(),
                pk_values,
            });
        // Patch the in-memory cell so the grid reflects the queued
        // change. The user still sees a `[N pending]` badge until they
        // commit.
        if let ResultState::Rows { rows, .. } =
            self.tabs[self.active_tab].results.active_state_mut()
        {
            if let Some(row_mut) = rows.get_mut(edit.row_index) {
                if let Some(cell) = row_mut.0.get_mut(edit.column_index) {
                    *cell = new_value;
                }
            }
        }
        self.tabs[self.active_tab].editing = None;
        self.tabs[self.active_tab].results.active_mut().edit = None;
        let total = self.tabs[self.active_tab].pending.len();
        self.status.message = format!("queued UPDATE on {} · {total} pending", target.display());
    }

    /// `Ctrl-X` — throw away every staged mutation.
    pub(super) fn discard_pending(&mut self) {
        let n = self.tabs[self.active_tab].pending.len();
        if n == 0 {
            self.status.message = "nothing pending to discard".into();
            return;
        }
        self.tabs[self.active_tab].pending.clear();
        self.tabs[self.active_tab].pending_preview = None;
        self.status.message = format!("discarded {n} pending mutation(s)");
    }

    /// Modal handler for the pending preview overlay. Owns scroll
    /// chords (`j`/`k`/`Ctrl-D`/`Ctrl-U`/`g`/`G`) plus the close
    /// shortcuts (`Esc`/`q`/`Ctrl-P`). `Ctrl-S` commit and `Ctrl-X`
    /// discard are forwarded to the regular Results-pane handlers so
    /// the user does not lose their muscle memory; both close the
    /// modal as a side effect of clearing the queue.
    pub(super) fn handle_pending_preview_key(&mut self, key: KeyEvent) {
        let total = self.tabs[self.active_tab].pending.len() as u16;
        let max_scroll = total.saturating_sub(1);
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                self.tabs[self.active_tab].pending_preview = None;
                self.status.message = "preview closed".into();
            }
            KeyCode::Char('p') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.toggle_pending_preview();
            }
            KeyCode::Char('s') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.commit_pending();
            }
            KeyCode::Char('x') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.discard_pending();
            }
            KeyCode::Char('j') | KeyCode::Down => {
                if let Some(state) = self.tabs[self.active_tab].pending_preview.as_mut() {
                    state.scroll = state.scroll.saturating_add(1).min(max_scroll);
                }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                if let Some(state) = self.tabs[self.active_tab].pending_preview.as_mut() {
                    state.scroll = state.scroll.saturating_sub(1);
                }
            }
            KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if let Some(state) = self.tabs[self.active_tab].pending_preview.as_mut() {
                    state.scroll = state.scroll.saturating_add(10).min(max_scroll);
                }
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if let Some(state) = self.tabs[self.active_tab].pending_preview.as_mut() {
                    state.scroll = state.scroll.saturating_sub(10);
                }
            }
            KeyCode::Char('g') => {
                if let Some(state) = self.tabs[self.active_tab].pending_preview.as_mut() {
                    state.scroll = 0;
                }
            }
            KeyCode::Char('G') => {
                if let Some(state) = self.tabs[self.active_tab].pending_preview.as_mut() {
                    state.scroll = max_scroll;
                }
            }
            _ => {}
        }
    }

    /// `Ctrl-P` — toggle the pending preview modal.
    pub(super) fn toggle_pending_preview(&mut self) {
        if self.tabs[self.active_tab].pending_preview.is_some() {
            self.tabs[self.active_tab].pending_preview = None;
            self.status.message = "preview closed".into();
            return;
        }
        if self.tabs[self.active_tab].pending.is_empty() {
            self.status.message = "nothing pending to preview".into();
            return;
        }
        self.tabs[self.active_tab].pending_preview = Some(PendingPreviewState::default());
        let n = self.tabs[self.active_tab].pending.len();
        self.status.message =
            format!("preview: {n} pending · Ctrl-S commit · Ctrl-X discard · Esc close");
    }

    /// `Ctrl-S` — flush every staged mutation inside one transaction.
    /// On any compile / dispatch error the transaction is rolled back
    /// and the queue is left intact so the user can inspect and fix.
    pub(super) fn commit_pending(&mut self) {
        let queue: PendingChanges = {
            let tab = &mut self.tabs[self.active_tab];
            if tab.pending.is_empty() {
                self.status.message = "nothing pending to commit".into();
                return;
            }
            tab.pending.clone()
        };
        let Some(session) = self.session.as_ref() else {
            self.status.message = "no active connection".into();
            return;
        };
        let dialect = session.dialect();
        let compiled: Vec<CompiledMutation> = match queue.compile_all(dialect) {
            Ok(v) => v,
            Err(e) => {
                self.status.message = format!("commit blocked: {e}");
                return;
            }
        };
        let target = match session.transaction.as_ref() {
            Some(txn) => RunTarget::Pinned(txn.conn.clone()),
            None => RunTarget::Pool(session.pool.clone()),
        };
        let started = std::time::Instant::now();
        let outcome = tokio::task::block_in_place(|| {
            Handle::current().block_on(execute_batch(target, compiled.clone()))
        });
        // L36: audit log — every committed mutation is journalled as
        // a separate HistoryEntry tagged with `source = "pending"` so
        // an auditor can both (a) replay the exact SQL the user sent
        // and (b) tell that it came from the staged-mutation pipeline
        // rather than the regular query editor.
        self.record_pending_audit(&compiled, &outcome, started.elapsed());
        match outcome {
            Ok(rows_affected) => {
                let n = compiled.len();
                self.tabs[self.active_tab].pending.clear();
                self.tabs[self.active_tab].pending_preview = None;
                self.status.message =
                    format!("committed {n} mutation(s) · {rows_affected} row(s) affected");
                // Re-run the current preview so the grid shows the
                // server's authoritative view (auto-increment PKs,
                // generated timestamps, deletions reflected, ...).
                self.refresh_current_preview();
            }
            Err(e) => {
                self.status.message = format!("commit failed: {e} — queue preserved");
            }
        }
    }

    /// Best-effort: write one [`narwhal_history::HistoryEntry`] per
    /// mutation. On failure the whole batch is journalled as
    /// `Outcome::Failed` with the engine error attached to *every*
    /// statement — it's not possible to tell from outside the
    /// transaction which one tripped the rollback, so we conservatively
    /// flag them all and let the auditor disambiguate from the error
    /// message.
    fn record_pending_audit(
        &self,
        compiled: &[CompiledMutation],
        outcome: &Result<u64, String>,
        elapsed: std::time::Duration,
    ) {
        let Some(journal) = self.history_journal.as_ref() else {
            return;
        };
        let Some(session) = self.session.as_ref() else {
            return;
        };
        let conn_id = session.config.id;
        let conn_name = session.config.name.clone();
        let driver = session.config.driver.clone();
        let elapsed_ms = u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX);
        let per_step_ms = elapsed_ms / (compiled.len().max(1) as u64);
        let entries: Vec<narwhal_history::HistoryEntry> = compiled
            .iter()
            .map(|m| {
                let mut entry = narwhal_history::HistoryEntry::success(m.sql.clone())
                    .with_connection(conn_id, conn_name.clone())
                    .with_driver(driver.clone())
                    .with_timing(per_step_ms)
                    .with_source("pending");
                if let Err(err) = outcome {
                    entry = entry.with_failure(err.clone());
                }
                entry
            })
            .collect();
        let journal = std::sync::Arc::clone(journal);
        // Fire-and-forget: the audit write must not block the UI even
        // if the disk is slow. Errors are logged at debug; the user
        // already got the commit status message.
        tokio::spawn(async move {
            for entry in &entries {
                if let Err(error) = journal.append(entry).await {
                    tracing::debug!(target: "narwhal::pending", error = %error, "pending audit append failed");
                }
            }
        });
    }

    /// Re-issue the currently visible `SELECT * FROM ...` preview, if
    /// any. Used after a successful commit to repaint the grid.
    fn refresh_current_preview(&mut self) {
        let target = {
            let tab = &self.tabs[self.active_tab];
            match tab.results.active_state() {
                ResultState::Rows {
                    source: Some(src), ..
                } => Some((src.schema.clone(), src.table.clone(), src.offset)),
                _ => None,
            }
        };
        if let Some((schema, table, offset)) = target {
            self.run_preview(&schema, &table, offset);
        }
    }

    // `set_edit_error` is implemented on the results-actions impl;
    // we just delegate to it here.
}

/// Execute every compiled mutation in declaration order inside a single
/// transaction (or savepoint, when called from inside an existing
/// transaction). Returns the *total* rows affected across the batch.
async fn execute_batch(
    target: RunTarget,
    compiled: Vec<CompiledMutation>,
) -> Result<u64, String> {
    match target {
        RunTarget::Pool(pool) => {
            let mut conn = pool
                .acquire()
                .await
                .map_err(|e| format!("acquire: {e}"))?;
            conn.begin().await.map_err(|e| format!("begin: {e}"))?;
            match run_all(&mut *conn, &compiled).await {
                Ok(n) => {
                    conn.commit().await.map_err(|e| format!("commit: {e}"))?;
                    Ok(n)
                }
                Err(e) => {
                    // Best-effort rollback; we still return the original
                    // error to the user.
                    let _ = conn.rollback().await;
                    Err(e)
                }
            }
        }
        RunTarget::Pinned(handle) => {
            let mut guard = handle.lock().await;
            // Inside an existing transaction we wrap the batch in a
            // savepoint so a failure here doesn't unwind the user's
            // outer transaction.
            let sp = "narwhal_pending";
            guard
                .savepoint(sp)
                .await
                .map_err(|e| format!("savepoint: {e}"))?;
            match run_all(&mut **guard, &compiled).await {
                Ok(n) => {
                    guard
                        .release_savepoint(sp)
                        .await
                        .map_err(|e| format!("release: {e}"))?;
                    Ok(n)
                }
                Err(e) => {
                    let _ = guard.rollback_to_savepoint(sp).await;
                    let _ = guard.release_savepoint(sp).await;
                    Err(e)
                }
            }
        }
    }
}

async fn run_all(
    conn: &mut dyn narwhal_core::Connection,
    compiled: &[CompiledMutation],
) -> Result<u64, String> {
    let mut total: u64 = 0;
    for (idx, m) in compiled.iter().enumerate() {
        let result = conn
            .execute(&m.sql, &m.params)
            .await
            .map_err(|e| format!("mutation #{}: {e}", idx + 1))?;
        let affected = result.rows_affected.unwrap_or(0);
        match m.expects {
            ExpectedRows::Insert => {
                // Most engines report ≥1 for a successful INSERT; a few
                // drivers (sqlite, some clickhouse paths) report 0 for
                // `INSERT ... DEFAULT VALUES`. We treat that as success
                // because the engine raised no error.
                total = total.saturating_add(affected.max(1));
            }
            ExpectedRows::Exactly(n) => {
                if affected != n {
                    return Err(format!(
                        "mutation #{}: expected {n} row(s) affected, got {affected}",
                        idx + 1
                    ));
                }
                total = total.saturating_add(affected);
            }
        }
    }
    Ok(total)
}
