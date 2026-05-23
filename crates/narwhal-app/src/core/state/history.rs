//! Ctrl+R history modal state.

use narwhal_history::HistoryEntry;

pub struct HistoryState {
    /// All entries loaded from the journal.
    pub entries: Vec<HistoryEntry>,
    /// Current filter string (case-insensitive substring).
    pub filter: String,
    /// Index into the filtered subset.
    pub selected: usize,
}

impl HistoryState {
    /// Return the subset of entries matching the current filter.
    pub fn visible_entries(&self) -> Vec<&HistoryEntry> {
        if self.filter.is_empty() {
            self.entries.iter().collect()
        } else {
            let needle = self.filter.to_lowercase();
            self.entries
                .iter()
                .filter(|e| e.sql.to_lowercase().contains(&needle))
                .collect()
        }
    }
}

