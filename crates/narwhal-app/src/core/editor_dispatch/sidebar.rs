//! Sidebar pane key handling and the table-preview / DDL hooks.

use crossterm::event::{KeyCode as CtKey, KeyEvent, KeyModifiers};
use narwhal_core::ColumnHeader;
use narwhal_tui::Pane;
use tracing::debug;

use crate::core::{AppCore, ResultState, RowSource, SidebarItem};
use crate::run::RunMode;

impl AppCore {
    pub(crate) fn click_sidebar_table(&mut self, sidebar_idx: usize) {
        let Some(item) = self.sidebar_items.get(sidebar_idx).cloned() else {
            return;
        };
        let SidebarItem::Table { schema, name, .. } = item else {
            return;
        };
        self.sidebar_index = sidebar_idx;
        self.run_preview(&schema, &name, 0);
    }

    /// Click on a result tab: switch to that result index.
    pub(crate) fn handle_sidebar_key(&mut self, key: KeyEvent) {
        // L24: paging step. The actual viewport size depends on the
        // terminal height; using a fixed step keeps the binding
        // predictable regardless of layout. Wheel events use a smaller
        // step (3) to feel closer to the OS scroll cadence.
        const PAGE_STEP: usize = 10;
        match key.code {
            CtKey::Char('j') | CtKey::Down if !self.sidebar_items.is_empty() => {
                self.sidebar_index = (self.sidebar_index + 1) % self.sidebar_items.len();
            }
            CtKey::Char('k') | CtKey::Up if !self.sidebar_items.is_empty() => {
                let len = self.sidebar_items.len();
                self.sidebar_index = (self.sidebar_index + len - 1) % len;
            }
            CtKey::PageDown | CtKey::Char('d')
                if key.modifiers.contains(KeyModifiers::CONTROL)
                    && !self.sidebar_items.is_empty() =>
            {
                let len = self.sidebar_items.len();
                self.sidebar_index = (self.sidebar_index + PAGE_STEP).min(len - 1);
            }
            CtKey::PageUp | CtKey::Char('u')
                if key.modifiers.contains(KeyModifiers::CONTROL)
                    && !self.sidebar_items.is_empty() =>
            {
                self.sidebar_index = self.sidebar_index.saturating_sub(PAGE_STEP);
            }
            CtKey::Home if !self.sidebar_items.is_empty() => {
                self.sidebar_index = 0;
            }
            CtKey::End if !self.sidebar_items.is_empty() => {
                self.sidebar_index = self.sidebar_items.len() - 1;
            }
            CtKey::Enter => self.activate_sidebar_selection(),
            CtKey::Char('o') => self.preview_sidebar_selection(),
            CtKey::Char('d') => self.ddl_sidebar_selection(),
            _ => {}
        }
    }

    /// L24: scroll the sidebar viewport by `delta` rows without moving
    /// the selection. Mouse-wheel handlers call this so the user can
    /// inspect off-screen rows before committing to a click.
    pub(crate) fn scroll_sidebar(&mut self, delta: isize) {
        if self.sidebar_items.is_empty() {
            return;
        }
        let max = self.sidebar_items.len().saturating_sub(1);
        let new = (self.sidebar_scroll as isize + delta).clamp(0, max as isize) as usize;
        self.sidebar_scroll = new;
    }

    pub(crate) fn preview_sidebar_selection(&mut self) {
        let Some(item) = self.sidebar_items.get(self.sidebar_index).cloned() else {
            return;
        };
        let SidebarItem::Table { schema, name, .. } = item else {
            self.status.message = "select a table to preview".into();
            return;
        };
        self.run_preview(&schema, &name, 0);
    }

    /// Pressing `d` with a sidebar table focused fetches the DDL and
    /// injects it into the editor at the cursor. No auto-run — the
    /// user inspects and decides.
    pub(crate) fn ddl_sidebar_selection(&mut self) {
        let Some(item) = self.sidebar_items.get(self.sidebar_index).cloned() else {
            return;
        };
        let SidebarItem::Table { schema, name, .. } = item else {
            self.status.message = "select a table to fetch DDL".into();
            return;
        };
        self.inject_ddl(&schema, &name);
    }

    /// Sprint 11 (Opus M1): sidebar DDL fetch goes through the meta
    /// channel so a slow `SHOW CREATE TABLE` (large views, `MySQL`
    /// `information_schema` lookups, `ClickHouse` `system.tables`) no
    /// longer freezes the UI. The result lands as
    /// [`MetaUpdate::InjectDdlReady`] tagged with the originating tab
    /// id; if the user closed the tab during the fetch the reply is
    /// dropped with a status message instead of writing DDL into an
    /// arbitrary tab (C5 invariant).
    pub(crate) fn inject_ddl(&mut self, schema: &str, name: &str) {
        let Some(session) = self.session.active.as_ref() else {
            self.status.message = "no active connection".into();
            return;
        };
        let pool = session.pool.clone();
        let schema_owned = schema.to_owned();
        let name_owned = name.to_owned();
        let tab_id = self.tabs[self.active_tab].id();
        let meta_tx = self.process.meta_tx.clone();
        self.status.message = format!("fetching DDL for {schema}.{name}…");
        tokio::spawn(async move {
            let result = async {
                let mut conn = pool
                    .acquire()
                    .await
                    .map_err(|e| narwhal_core::Error::Connection(e.to_string()))?;
                conn.fetch_ddl(&schema_owned, &name_owned).await
            }
            .await;
            let update = match result {
                Ok(ddl) => crate::meta::MetaUpdate::InjectDdlReady {
                    tab_id,
                    schema: schema_owned,
                    name: name_owned,
                    ddl,
                },
                Err(e) => crate::meta::MetaUpdate::MetaFailed {
                    message: format!("DDL fetch failed: {e}"),
                },
            };
            let _ = meta_tx.send(update).await;
        });
    }

    /// Dispatch a `SELECT * FROM schema.table LIMIT n OFFSET k` and attach
    /// the table's schema as the result's row source so cell edits and
    /// pagination work.
    pub(crate) fn run_preview(&mut self, schema: &str, table: &str, offset: usize) {
        let Some(session) = self.session.active.as_ref() else {
            self.status.message = "no active connection".into();
            return;
        };
        let dialect = session.dialect();
        let limit = self.tabs[self.active_tab].page_size;
        let pool = session.pool.clone();
        let schema_owned = schema.to_owned();
        let name_owned = table.to_owned();
        // Sprint 11 (Opus M1) deferred: the row-preview pre-flight
        // describe is on the same hot path as the in-result describe
        // above and shares the trade-off — the schema is attached to
        // the freshly-dispatched run via `RunUpdate::ResultReady`
        // and the dispatch happens *after* the describe completes
        // (so the column metadata is already on the row source).
        // Splitting these requires a two-stage RunRequest API.
        let described = tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(async move {
                let mut conn = pool
                    .acquire()
                    .await
                    .map_err(|e| narwhal_core::Error::Connection(e.to_string()))?;
                conn.describe_table(&schema_owned, &name_owned).await
            })
        });
        let source = match described {
            Ok(ts) => {
                let columns = ts.columns;
                // Cache column names for completion.
                if let Some(session) = self.session.active.as_mut() {
                    session.column_cache.insert(
                        table.to_ascii_lowercase(),
                        (
                            schema.to_owned(),
                            columns
                                .iter()
                                .map(|c| ColumnHeader {
                                    name: c.name.clone(),
                                    data_type: c.data_type.clone(),
                                })
                                .collect(),
                        ),
                    );
                }
                Some(RowSource {
                    schema: schema.to_owned(),
                    table: table.to_owned(),
                    columns,
                    offset,
                    limit,
                })
            }
            Err(error) => {
                debug!(target: "narwhal::app", error = %error, "describe_table for preview failed; rows will be read-only");
                None
            }
        };
        let sql = crate::ddl::preview_query_paged(schema, table, limit, offset, dialect);
        self.tabs[self.active_tab].pending_source = source;
        self.dispatch_batch(vec![sql], RunMode::Execute);
        self.focus = Pane::Results;
    }

    pub(crate) fn next_page(&mut self) {
        let Some((schema, table, offset)) = self.current_preview_target() else {
            self.status.message = "no preview to paginate; select a table first".into();
            return;
        };
        let limit = self.tabs[self.active_tab].page_size;
        self.run_preview(&schema, &table, offset + limit);
    }

    pub(crate) fn prev_page(&mut self) {
        let Some((schema, table, offset)) = self.current_preview_target() else {
            self.status.message = "no preview to paginate; select a table first".into();
            return;
        };
        if offset == 0 {
            self.status.message = "already on the first page".into();
            return;
        }
        let limit = self.tabs[self.active_tab].page_size;
        let new_offset = offset.saturating_sub(limit);
        self.run_preview(&schema, &table, new_offset);
    }

    pub(crate) fn set_page_size(&mut self, size: usize) {
        self.tabs[self.active_tab].page_size = size;
        self.status.message = format!("page size set to {size}");
    }

    pub(crate) fn current_preview_target(&self) -> Option<(String, String, usize)> {
        match self.tabs[self.active_tab].results.active_state() {
            ResultState::Rows {
                source: Some(s), ..
            } => Some((s.schema.clone(), s.table.clone(), s.offset)),
            _ => None,
        }
    }

    pub(crate) fn activate_sidebar_selection(&mut self) {
        let Some(item) = self.sidebar_items.get(self.sidebar_index).cloned() else {
            return;
        };
        match item {
            SidebarItem::Connection { name, .. } => self.open_named(&name),
            SidebarItem::Schema { .. } => {}
            SidebarItem::Table { schema, name, .. } => {
                self.describe_table_into_result(&schema, &name);
            }
        }
    }

    /// Sprint 11 (Opus M1) deferred: the describe-table path keeps
    /// its `block_in_place` because the result mutates the active
    /// tab's result pane in-place — column lists, index summaries,
    /// foreign-key edges — and the calling sidebar key-handler
    /// expects the new result to be visible by the time it returns
    /// (so a follow-up keypress that scrolls into the new pane sees
    /// the data). Routing this through the meta channel requires
    /// either a `MetaUpdate::DescribeReady` variant *plus* a
    /// "transitioning" UI state for the sidebar — deferred to the
    /// same epic that owns the `handle_key` async refactor. The
    /// multi-thread runtime absorbs the freeze (typical < 30 ms).
    pub(crate) fn describe_table_into_result(&mut self, schema: &str, name: &str) {
        let Some(session) = self.session.active.as_ref() else {
            self.status.message = "no active connection".into();
            return;
        };
        let pool = session.pool.clone();
        let schema_owned = schema.to_owned();
        let name_owned = name.to_owned();
        let result = tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(async move {
                let mut conn = pool
                    .acquire()
                    .await
                    .map_err(|e| narwhal_core::Error::Connection(e.to_string()))?;
                conn.describe_table(&schema_owned, &name_owned).await
            })
        });
        match result {
            Ok(ts) => {
                let col_count = ts.columns.len();
                let idx_count = ts.indexes.len();
                let fk_count = ts.foreign_keys.len();
                let table_schema = ts.table.schema.clone();
                let table_name = ts.table.name.clone();
                let columns = ts.columns.clone();
                // Cache column names for completion.
                if let Some(session) = self.session.active.as_mut() {
                    session.column_cache.insert(
                        table_name.to_ascii_lowercase(),
                        (
                            table_schema.clone(),
                            columns
                                .iter()
                                .map(|c| ColumnHeader {
                                    name: c.name.clone(),
                                    data_type: c.data_type.clone(),
                                })
                                .collect(),
                        ),
                    );
                }
                self.tabs[self.active_tab].results.active_mut().reset();
                self.status.message = format!(
                    "{table_schema}.{table_name}: {col_count} cols·{idx_count} idx·{fk_count} fk"
                );
                *self.tabs[self.active_tab].results.active_state_mut() = ResultState::TableDetail {
                    schema: ts,
                    active_meta_tab: narwhal_tui::MetaTab::default(),
                };
                // L36: hand focus to the results pane so 1–5 land on
                // the new tab strip without the user first having to
                // cycle panes with Ctrl-W.
                self.focus = narwhal_tui::Pane::Results;
            }
            Err(error) => {
                self.status.message = format!("describe failed: {error}");
            }
        }
    }

    // handle_results_key, selected_original_row, yank_cell, yank_row,
    // start_cell_edit, handle_cell_edit_key, sync_edit_view, commit_cell_edit,
    // set_edit_error, start_search, toggle_sort, open_filter_prompt,
    // refresh_search_matches, advance_search, jump_to_current_match,
    // open_cell_popup, open_row_detail, handle_row_detail_key moved to
    // `core::results_actions` (L21).
}
