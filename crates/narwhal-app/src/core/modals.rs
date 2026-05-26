//! Modal-overlay handlers extracted from `core.rs` (L21).
//!
//! Bundles every key handler / lifecycle method that drives a
//! full-screen modal:
//! - help (F1, `?`)
//! - history (Ctrl+R)
//! - snippets (`:snippets`, `:save`, `:load`, `:rm-snippet`)
//! - wizard (`:add`)
use crossterm::event::{KeyCode as CtKey, KeyEvent, KeyModifiers};

use super::{AppCore, SidebarItem, SnippetsModal};
use crate::meta::MetaRequest;
use crate::run::RunMode;
use crate::wizard::DRIVERS;

impl AppCore {
    pub async fn open_help(&mut self) {
        self.modals.help_open = true;
    }

    pub(super) async fn toggle_help(&mut self) {
        self.modals.help_open = !self.modals.help_open;
    }

    /// Open the Ctrl+R history modal. Dispatches a background
    /// load via the meta channel (H11) so the UI stays responsive.
    pub async fn open_history(&mut self) {
        let Some(_journal) = &self.session.history_journal else {
            self.ui.status.message = "history disabled".into();
            return;
        };
        self.dispatch_meta(MetaRequest::LoadHistory {
            limit: narwhal_tui::constants::HISTORY_LOAD_LIMIT,
        })
        .await;
        self.ui.status.message = "loading history…".into();
    }

    pub(super) async fn close_history(&mut self) {
        self.modals.history = None;
    }

    /// Handle key events while the history modal is open.
    pub(super) async fn handle_history_key(&mut self, key: KeyEvent) {
        let Some(state) = self.modals.history.as_mut() else {
            return;
        };
        match key.code {
            CtKey::Esc => {
                self.close_history().await;
                self.ui.status.message = "history closed".into();
            }
            CtKey::Up | CtKey::Char('k')
                if key.modifiers.is_empty() || key.modifiers.contains(KeyModifiers::CONTROL) =>
            {
                let visible = state.visible_entries();
                if !visible.is_empty() {
                    state.selected = (state.selected + visible.len() - 1) % visible.len();
                }
            }
            CtKey::Down | CtKey::Char('j')
                if key.modifiers.is_empty() || key.modifiers.contains(KeyModifiers::CONTROL) =>
            {
                let visible = state.visible_entries();
                if !visible.is_empty() {
                    state.selected = (state.selected + 1) % visible.len();
                }
            }
            CtKey::Enter => {
                let sql = {
                    let visible = state.visible_entries();
                    visible.get(state.selected).map(|e| e.sql.clone())
                };
                if let Some(sql) = sql {
                    let shift = key.modifiers.contains(KeyModifiers::SHIFT);
                    self.close_history().await;
                    self.ui.tabs[self.ui.active_tab].editor.insert_str(&sql);
                    if shift {
                        self.dispatch_current_statement(RunMode::Execute).await;
                    } else {
                        self.ui.status.message =
                            format!("inserted {} char(s) from history", sql.len());
                    }
                } else {
                    self.close_history().await;
                }
            }
            CtKey::Backspace => {
                state.filter.pop();
                state.selected = 0;
            }
            CtKey::Char(c) => {
                state.filter.push(c);
                state.selected = 0;
            }
            _ => {}
        }
    }

    /// Open the `:snippets` modal. Reads the snippet list from the store.
    pub(super) async fn open_snippets_modal(&mut self) {
        match self.session.snippet_store.list() {
            Ok(entries) => {
                if entries.is_empty() {
                    self.ui.status.message = "no saved snippets; use :save <name> first".into();
                    return;
                }
                self.modals.snippets = Some(SnippetsModal {
                    entries,
                    selected: 0,
                });
                self.ui.status.message = "snippets: ↑↓/jk navigate · Enter load · Esc close".into();
            }
            Err(error) => {
                self.ui.status.message = format!("snippets: could not list: {error}");
            }
        }
    }

    pub(super) async fn close_snippets_modal(&mut self) {
        self.modals.snippets = None;
    }

    /// Handle key events while the snippets modal is open.
    pub(super) async fn handle_snippets_key(&mut self, key: KeyEvent) {
        let Some(modal) = self.modals.snippets.as_mut() else {
            return;
        };
        match key.code {
            CtKey::Esc => {
                self.close_snippets_modal().await;
                self.ui.status.message = "snippets closed".into();
            }
            CtKey::Up | CtKey::Char('k')
                if (key.modifiers.is_empty() || key.modifiers.contains(KeyModifiers::CONTROL))
                    && !modal.entries.is_empty() =>
            {
                modal.selected = (modal.selected + modal.entries.len() - 1) % modal.entries.len();
            }
            CtKey::Down | CtKey::Char('j')
                if (key.modifiers.is_empty() || key.modifiers.contains(KeyModifiers::CONTROL))
                    && !modal.entries.is_empty() =>
            {
                modal.selected = (modal.selected + 1) % modal.entries.len();
            }
            CtKey::Enter => {
                let name = self
                    .modals
                    .snippets
                    .as_ref()
                    .and_then(|m| m.entries.get(m.selected).cloned());
                self.close_snippets_modal().await;
                if let Some(name) = name {
                    self.load_snippet_by_name(&name).await;
                } else {
                    self.ui.status.message = "snippets closed".into();
                }
            }
            _ => {}
        }
    }

    /// Load a snippet by name into a new editor tab.
    pub(super) async fn load_snippet_by_name(&mut self, name: &str) {
        match self.session.snippet_store.load(name) {
            Ok(sql) => {
                self.new_tab().await;
                self.ui.tabs[self.ui.active_tab].editor.insert_str(&sql);
                self.ui.tabs[self.ui.active_tab].name = name.to_owned();
                self.ui.status.message = format!("loaded snippet '{name}' ({} char(s))", sql.len());
            }
            Err(error) => {
                self.ui.status.message = format!("load failed: {error}");
            }
        }
    }

    /// Save the current editor buffer as a named snippet.
    pub(super) async fn save_snippet(&mut self, name: &str) {
        let sql = self.ui.tabs[self.ui.active_tab].editor.entire_text();
        if sql.trim().is_empty() {
            self.ui.status.message = "editor is empty; nothing to save".into();
            return;
        }
        match self.session.snippet_store.save(name, &sql) {
            Ok(()) => {
                self.ui.status.message = format!("saved snippet '{name}'");
            }
            Err(error) => {
                self.ui.status.message = format!("save failed: {error}");
            }
        }
    }

    /// Remove a named snippet.
    pub(super) async fn remove_snippet(&mut self, name: &str) {
        match self.session.snippet_store.remove(name) {
            Ok(()) => {
                self.ui.status.message = format!("removed snippet '{name}'");
            }
            Err(error) => {
                self.ui.status.message = format!("rm-snippet failed: {error}");
            }
        }
    }

    pub(super) async fn cancel_wizard(&mut self) {
        if self.modals.wizard.take().is_some() {
            self.modals.wizard_error = None;
            self.ui.status.message = "add cancelled".into();
        }
    }

    pub(super) async fn commit_wizard(&mut self) {
        let Some(wizard) = self.modals.wizard.as_ref() else {
            return;
        };
        // Capture the editing-id once so the rest of the routine can rely
        // on it without re-borrowing `self.modals.wizard`.
        let existing_id = wizard.existing_id;
        match wizard.build() {
            Err(error) => {
                self.modals.wizard_error = Some(error);
            }
            Ok(built) => {
                // Name-collision check: skip when the candidate is the
                // very entry being edited; reject when any *other* entry
                // already owns the chosen name.
                if self
                    .session
                    .connections
                    .connections
                    .iter()
                    .any(|c| c.name == built.config.name && Some(c.id) != existing_id)
                {
                    self.modals.wizard_error = Some(format!(
                        "a connection named '{}' already exists",
                        built.config.name
                    ));
                    return;
                }
                let connection_id = built.config.id;
                let secret = built.password.clone();
                // Keep the previous entry around so we can restore it if
                // the on-disk save fails mid-flight.
                let previous = if let Some(id) = existing_id {
                    if let Some(pos) = self
                        .session
                        .connections
                        .connections
                        .iter()
                        .position(|c| c.id == id)
                    {
                        let prev = self.session.connections.connections.remove(pos);
                        self.session
                            .connections
                            .connections
                            .insert(pos, built.config.clone());
                        Some((pos, prev))
                    } else {
                        self.session
                            .connections
                            .connections
                            .push(built.config.clone());
                        None
                    }
                } else {
                    self.session
                        .connections
                        .connections
                        .push(built.config.clone());
                    None
                };
                if let Some(path) = self.session.connections_path.as_ref() {
                    if let Err(error) = self.session.connections.save(path) {
                        self.modals.wizard_error = Some(format!("could not save: {error}"));
                        // Roll back the in-memory mutation so the on-disk file
                        // remains the source of truth.
                        match previous {
                            Some((pos, prev)) => {
                                self.session.connections.connections.remove(pos);
                                self.session.connections.connections.insert(pos, prev);
                            }
                            None => {
                                self.session.connections.connections.pop();
                            }
                        }
                        return;
                    }
                }
                if let Some(secret) = secret {
                    // Sprint 9 (H7): keep this synchronous on purpose.
                    // The user just pressed Enter to commit the
                    // wizard; surfacing the keyring outcome inline lets
                    // us populate `wizard_error` *before* the modal
                    // closes so the user sees "saved, but keyring
                    // failed" instead of a silent dismissal. The
                    // expected duration is sub-second (libsecret is
                    // already unlocked) and the wizard view is the
                    // user's mental "loading" surface for the entire
                    // commit step.
                    if let Err(error) = self.deps.credentials.set(connection_id, secret).await {
                        // The connection is still saved; just warn the user
                        // that the secret didn't make it to the keyring.
                        self.modals.wizard_error = Some(format!(
                            "saved, but storing the password in the keyring failed: {error}"
                        ));
                    }
                }
                self.modals.wizard = None;
                self.modals.wizard_error = None;
                self.rebuild_sidebar();
                let name = built.config.name;
                self.ui.status.message = if existing_id.is_some() {
                    format!("connection '{name}' updated")
                } else {
                    format!("connection '{name}' saved")
                };
                // Pre-select the new connection in the sidebar.
                if let Some(idx) = self.ui.sidebar_items.iter().position(|i| match i {
                    SidebarItem::Connection { name: n, .. } => n == &name,
                    _ => false,
                }) {
                    self.ui.sidebar_index = idx;
                }
            }
        }
    }

    pub(super) async fn handle_wizard_key(&mut self, key: KeyEvent) {
        let Some(wizard) = self.modals.wizard.as_mut() else {
            return;
        };
        // Path-style fields hijack Tab for filesystem completion;
        // Shift-Tab / arrows still walk between fields so users aren't
        // trapped once they have what they wanted.
        if matches!(key.code, CtKey::Tab) && wizard.focused_is_path() {
            let outcome = wizard.complete_focused_path();
            self.ui.status.message = match outcome {
                crate::wizard::PathCompletion::NoMatch => "no match".into(),
                crate::wizard::PathCompletion::Single => "completed".into(),
                crate::wizard::PathCompletion::Multiple { count, samples } => {
                    let preview = samples.join("  ");
                    if count > samples.len() {
                        format!("{count} matches: {preview}  …")
                    } else {
                        format!("{count} matches: {preview}")
                    }
                }
            };
            return;
        }
        match key.code {
            CtKey::Esc => self.cancel_wizard().await,
            CtKey::Tab | CtKey::Down => wizard.next_focus(),
            CtKey::BackTab | CtKey::Up => wizard.prev_focus(),
            CtKey::Left if wizard.focused == 0 => wizard.cycle_driver(-1),
            CtKey::Right if wizard.focused == 0 => wizard.cycle_driver(1),
            CtKey::Enter => self.commit_wizard().await,
            CtKey::Backspace => wizard.pop_char(),
            CtKey::Char(c) => {
                if wizard.focused == 0 {
                    // Allow first-letter shortcuts on the driver row.
                    if let Some(idx) = DRIVERS.iter().position(|d| d.starts_with(c)) {
                        wizard.driver_index = idx;
                        wizard.cycle_driver(0);
                    }
                } else {
                    wizard.push_char(c);
                }
            }
            _ => {}
        }
    }
}
