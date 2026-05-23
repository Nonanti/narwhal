//! `:snippets` modal state.

pub struct SnippetsModal {
    /// Sorted list of snippet names.
    pub entries: Vec<String>,
    /// Index of the currently selected entry.
    pub selected: usize,
}
