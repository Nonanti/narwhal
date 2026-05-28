//! `:goto` fuzzy schema navigator.
//!
//! Opens a centred modal that lists every table/view across every
//! loaded session. The user types to fuzzy-match, Up/Down (or
//! Ctrl-J/K) to move the cursor, Enter to insert
//! `schema.table` at the editor cursor, Esc to cancel.
//!
//! Re-indexing happens once per open. Sessions of typical size
//! (\u2264 ~10k objects) re-index in microseconds; the per-keystroke cost
//! is then bounded by Nucleo's matcher (which Helix uses on much
//! larger corpora without complaint).

use crossterm::event::{KeyCode as CtKey, KeyEvent, KeyModifiers};
use nucleo_matcher::{Config as MatcherConfig, Matcher};

use super::{AppCore, GotoCorpusCache, GotoEntry, GotoModal};

impl AppCore {
    /// Build a fresh corpus from the active session's schema listing
    /// and open the navigator modal. If no connection is open we
    /// surface a status hint instead of an empty modal.
    pub(super) async fn open_goto_modal(&mut self) {
        let Some(session) = self.session.active.as_ref() else {
            self.ui.status.message = "goto: no active connection".into();
            return;
        };
        let conn_id = session.config.id;
        let schemas_version = session.schemas_version;
        let conn_name = session.config.name.clone();

        // m-7: reuse a previously-built corpus when the active
        // session and its schema version are unchanged. This
        // matters on big production schemas (10 k+ tables) where
        // the per-open Vec build is microseconds-to-milliseconds
        // and dominates the modal-open feel.
        let cache_hit =
            self.session.goto_corpus_cache.as_ref().is_some_and(|c| {
                c.connection_id == conn_id && c.schemas_version == schemas_version
            });

        let corpus: Vec<GotoEntry> = if cache_hit {
            // Safe: cache_hit checks Some + matching keys.
            self.session
                .goto_corpus_cache
                .as_ref()
                .map(|c| c.corpus.clone())
                .unwrap_or_default()
        } else {
            let mut new_corpus: Vec<GotoEntry> = Vec::new();
            for (schema, tables) in &session.schemas {
                for tbl in tables {
                    new_corpus.push(GotoEntry::new(
                        &conn_name,
                        &schema.name,
                        &tbl.name,
                        tbl.kind,
                    ));
                }
            }
            if new_corpus.is_empty() {
                self.ui.status.message = "goto: no schemas loaded yet (try :refresh first)".into();
                return;
            }
            self.session.goto_corpus_cache = Some(GotoCorpusCache {
                connection_id: conn_id,
                schemas_version,
                corpus: new_corpus.clone(),
            });
            new_corpus
        };
        if corpus.is_empty() {
            self.ui.status.message = "goto: no schemas loaded yet (try :refresh first)".into();
            return;
        }
        self.modals.goto = Some(GotoModal::new(corpus));
        self.ui.status.message = "goto: type to filter, Enter inserts, Esc cancels".into();
    }

    /// Handle a key while the goto modal owns the foreground.
    pub(super) async fn handle_goto_key(&mut self, key: KeyEvent) {
        if key.code == CtKey::Esc {
            self.modals.goto = None;
            self.ui.status.message = "cancelled".into();
            return;
        }
        if key.code == CtKey::Enter {
            let Some(modal) = self.modals.goto.as_ref() else {
                return;
            };
            let Some(entry) = modal.current_entry() else {
                self.ui.status.message = "goto: no match".into();
                return;
            };
            let insertion = entry.insertion();
            self.modals.goto = None;
            // Insert the qualified name at the editor cursor in the
            // active tab. The user can then keep typing (`SELECT * FROM `
            // is most likely already in the buffer).
            let tab = &mut self.ui.tabs[self.ui.active_tab];
            tab.editor.insert_str(&insertion);
            self.ui.status.message = format!("goto: inserted {insertion}");
            return;
        }

        // Up / Ctrl-K / Ctrl-P move up. Down / Ctrl-J / Ctrl-N move down.
        let delta: Option<isize> = match key.code {
            CtKey::Up => Some(-1),
            CtKey::Down => Some(1),
            CtKey::Char('k' | 'p') if key.modifiers.contains(KeyModifiers::CONTROL) => Some(-1),
            CtKey::Char('j' | 'n') if key.modifiers.contains(KeyModifiers::CONTROL) => Some(1),
            _ => None,
        };
        if let Some(d) = delta {
            if let Some(modal) = self.modals.goto.as_mut() {
                modal.move_cursor(d);
            }
            return;
        }

        // Backspace pops a char and re-ranks.
        if key.code == CtKey::Backspace {
            let mut matcher = Matcher::new(MatcherConfig::DEFAULT);
            if let Some(modal) = self.modals.goto.as_mut() {
                modal.query.pop();
                modal.rerank(&mut matcher);
            }
            return;
        }

        // Ctrl-U clears the query in one stroke.
        if key.code == CtKey::Char('u') && key.modifiers.contains(KeyModifiers::CONTROL) {
            let mut matcher = Matcher::new(MatcherConfig::DEFAULT);
            if let Some(modal) = self.modals.goto.as_mut() {
                modal.query.clear();
                modal.rerank(&mut matcher);
            }
            return;
        }

        // Any printable char (only Shift modifier tolerated) extends
        // the query and triggers a re-rank.
        if let CtKey::Char(c) = key.code {
            let only_shift = key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT;
            if only_shift {
                let mut matcher = Matcher::new(MatcherConfig::DEFAULT);
                if let Some(modal) = self.modals.goto.as_mut() {
                    modal.query.push(c);
                    modal.rerank(&mut matcher);
                }
            }
        }
    }
}
