//! `:goto` fuzzy schema navigator state (v1.1 #1).
//!
//! Indexes every visible (schema, table/view) across all currently
//! loaded sessions and runs the user's query through a Nucleo fuzzy
//! matcher (the same algorithm Helix uses). The state is rebuilt
//! every time the modal opens — schema lists are small enough
//! (typically a few hundred entries; tens of thousands at the
//! extreme) that a re-index per open is cheaper than maintaining a
//! live cache and far simpler.
//!
//! The modal owns only its query buffer + ranked matches.
//! Re-indexing on open keeps the data structurally simple and lets
//! the user's `:refresh` invalidate the index for free.

use narwhal_core::TableKind;

/// One indexable navigation target. Currently only tables and views;
/// extending to columns / sequences / functions is a follow-up.
#[derive(Debug, Clone)]
pub struct GotoEntry {
    /// `connection.schema.table` \u2014 used both as the haystack for the
    /// matcher and as the display label.
    pub qualified: String,
    /// Connection name (display + insertion).
    pub connection: String,
    /// Schema name.
    pub schema: String,
    /// Table or view name.
    pub table: String,
    /// View / system table flag. Drives the badge in the modal.
    pub kind: TableKind,
}

impl GotoEntry {
    #[must_use]
    pub fn new(connection: &str, schema: &str, table: &str, kind: TableKind) -> Self {
        Self {
            qualified: format!("{connection}.{schema}.{table}"),
            connection: connection.to_owned(),
            schema: schema.to_owned(),
            table: table.to_owned(),
            kind,
        }
    }

    /// `"schema.table"` \u2014 what we insert at the editor cursor.
    #[must_use]
    pub fn insertion(&self) -> String {
        format!("{}.{}", self.schema, self.table)
    }
}

/// Result of one fuzzy-match pass: the index into `corpus` plus the
/// match score (higher = better). Bundled so the renderer can show
/// only the top-N matches without re-scoring.
#[derive(Debug, Clone, Copy)]
pub struct GotoMatch {
    pub entry_idx: usize,
    pub score: u32,
}

/// Modal state. Owns the corpus (rebuilt on open), the user's query,
/// and the current ranked matches.
pub struct GotoModal {
    /// Full corpus of navigation targets, indexed once on open.
    pub corpus: Vec<GotoEntry>,
    /// User's current query. Mutated by keypresses.
    pub query: String,
    /// Ranked matches against [`Self::query`]. Sorted high \u2192 low score.
    pub matches: Vec<GotoMatch>,
    /// Index into [`Self::matches`] of the highlighted row.
    pub cursor: usize,
}

impl GotoModal {
    /// Build a modal from a list of entries. The empty query case
    /// shows every entry in original order so the user can scroll
    /// even before typing.
    #[must_use]
    pub fn new(corpus: Vec<GotoEntry>) -> Self {
        let matches = (0..corpus.len())
            .map(|i| GotoMatch {
                entry_idx: i,
                score: 0,
            })
            .collect();
        Self {
            corpus,
            query: String::new(),
            matches,
            cursor: 0,
        }
    }

    /// Re-rank the corpus against [`Self::query`] using a Nucleo
    /// matcher. Caller is expected to construct the matcher once and
    /// pass it in so per-keystroke cost is amortised.
    ///
    /// Resets the cursor to 0 because the previously selected item
    /// may have moved.
    pub fn rerank(&mut self, matcher: &mut nucleo_matcher::Matcher) {
        use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};

        if self.query.is_empty() {
            self.matches = (0..self.corpus.len())
                .map(|i| GotoMatch {
                    entry_idx: i,
                    score: 0,
                })
                .collect();
            self.cursor = 0;
            return;
        }

        let pattern = Pattern::parse(&self.query, CaseMatching::Smart, Normalization::Smart);
        // C1: Use `Utf32Str::new` so non-ASCII identifiers (e.g.
        // "ürünler" tables) are accepted. The previous
        // `Utf32Str::Ascii(.as_bytes())` shortcut interpreted UTF-8
        // bytes as ASCII code units, which scored wrong at best and
        // crashed inside nucleo's char iterator at worst. The buffer
        // is allocated per-iteration; `Utf32Str::new` re-uses the
        // ASCII fast path when the haystack is in fact ASCII.
        let mut scored: Vec<GotoMatch> = self
            .corpus
            .iter()
            .enumerate()
            .filter_map(|(i, e)| {
                let mut buf = Vec::new();
                let haystack = nucleo_matcher::Utf32Str::new(&e.qualified, &mut buf);
                pattern.score(haystack, matcher).map(|s| GotoMatch {
                    entry_idx: i,
                    score: s,
                })
            })
            .collect();
        scored.sort_unstable_by_key(|m| std::cmp::Reverse(m.score));
        self.matches = scored;
        self.cursor = 0;
    }

    /// Currently highlighted entry, if any.
    #[must_use]
    pub fn current_entry(&self) -> Option<&GotoEntry> {
        let m = self.matches.get(self.cursor)?;
        self.corpus.get(m.entry_idx)
    }

    /// Move the cursor by `delta`, clamped to the visible match range.
    pub fn move_cursor(&mut self, delta: isize) {
        let len = self.matches.len();
        if len == 0 {
            self.cursor = 0;
            return;
        }
        let new = (self.cursor as isize + delta).rem_euclid(len as isize);
        self.cursor = new as usize;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> Vec<GotoEntry> {
        vec![
            GotoEntry::new("prod", "public", "users", TableKind::Table),
            GotoEntry::new("prod", "public", "user_sessions", TableKind::Table),
            GotoEntry::new("prod", "public", "orders", TableKind::Table),
            GotoEntry::new("prod", "billing", "invoices", TableKind::Table),
            GotoEntry::new("staging", "public", "users", TableKind::Table),
        ]
    }

    #[test]
    fn empty_query_shows_all() {
        let modal = GotoModal::new(fixture());
        assert_eq!(modal.matches.len(), 5);
    }

    #[test]
    fn fuzzy_ranks_user_above_orders() {
        let mut matcher = nucleo_matcher::Matcher::new(nucleo_matcher::Config::DEFAULT);
        let mut modal = GotoModal::new(fixture());
        modal.query = "user".into();
        modal.rerank(&mut matcher);
        let top = modal.current_entry().expect("at least one match");
        assert!(top.table.contains("user"));
    }

    #[test]
    fn fuzzy_handles_non_ascii_identifiers() {
        // C1 regression: previously panicked on the Unicode codepoint
        // when fed UTF-8 bytes via Utf32Str::Ascii.
        let mut matcher = nucleo_matcher::Matcher::new(nucleo_matcher::Config::DEFAULT);
        let corpus = vec![
            GotoEntry::new("prod", "public", "ürünler", TableKind::Table),
            GotoEntry::new("prod", "public", "使用者", TableKind::Table),
            GotoEntry::new("prod", "public", "orders", TableKind::Table),
        ];
        let mut modal = GotoModal::new(corpus);
        modal.query = "ür".into();
        modal.rerank(&mut matcher);
        let top = modal.current_entry().expect("at least one match");
        assert_eq!(top.table, "ürünler");
    }

    #[test]
    fn cursor_wraps() {
        let mut modal = GotoModal::new(fixture());
        modal.cursor = 0;
        modal.move_cursor(-1);
        assert_eq!(modal.cursor, modal.matches.len() - 1);
        modal.move_cursor(1);
        assert_eq!(modal.cursor, 0);
    }

    #[test]
    fn insertion_is_schema_dot_table() {
        let e = GotoEntry::new("prod", "billing", "invoices", TableKind::Table);
        assert_eq!(e.insertion(), "billing.invoices");
    }
}
