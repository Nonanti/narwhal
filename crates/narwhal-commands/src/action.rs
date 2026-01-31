//! Application-level actions decoupled from raw key events.
//!
//! The dispatch path in `narwhal-app` turns a raw [`crossterm::event::KeyEvent`]
//! into an [`Action`] through the configured [`crate::keymap::Keymap`] and then
//! routes that action to the appropriate handler. This indirection enables:
//!
//! - User-defined keybindings (TOML overrides in `config.toml`).
//! - Unified test fixtures that invoke handlers by name rather than synthesising
//!   key events.
//! - Future plugin commands that bind to the same action vocabulary.
//!
//! Actions are **grouped** by the UI surface they affect (see [`KeyGroup`]). The
//! same chord (`j`) can mean different things in different groups; the resolver
//! looks up the chord *only* inside the active group, so collisions are by
//! design impossible.

use serde::{Deserialize, Serialize};

/// Logical UI surface that owns a set of keybindings.
///
/// At any given moment exactly one group is active for key resolution. Modals
/// (cell popup, row detail, pending preview) sit on top of the base groups
/// and intercept input until they are dismissed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum KeyGroup {
    /// Bindings that fire regardless of which pane is focused.
    Global,
    /// Bindings active when the editor pane owns focus.
    Editor,
    /// Bindings active when the sidebar pane owns focus.
    Sidebar,
    /// Bindings active when the results pane owns focus.
    Results,
    /// Bindings active inside the row detail modal.
    RowDetail,
    /// Bindings active inside the cell popup overlay.
    CellPopup,
    /// Bindings active inside the JSON viewer modal.
    JsonViewer,
    /// Bindings active inside the pending-changes preview modal.
    PendingPreview,
}

impl KeyGroup {
    /// Stable identifier used as the TOML section name (e.g. `[keymap.results]`).
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Global => "global",
            Self::Editor => "editor",
            Self::Sidebar => "sidebar",
            Self::Results => "results",
            Self::RowDetail => "row-detail",
            Self::CellPopup => "cell-popup",
            Self::JsonViewer => "json-viewer",
            Self::PendingPreview => "pending-preview",
        }
    }

    /// Inverse of [`Self::as_str`]; useful when loading TOML.
    pub fn from_str_opt(s: &str) -> Option<Self> {
        match s {
            "global" => Some(Self::Global),
            "editor" => Some(Self::Editor),
            "sidebar" => Some(Self::Sidebar),
            "results" => Some(Self::Results),
            "row-detail" | "row_detail" => Some(Self::RowDetail),
            "cell-popup" | "cell_popup" => Some(Self::CellPopup),
            "json-viewer" | "json_viewer" => Some(Self::JsonViewer),
            "pending-preview" | "pending_preview" => Some(Self::PendingPreview),
            _ => None,
        }
    }
}

/// Every action the user can trigger from a keybinding.
///
/// Variants are flat (no nested enums) so a single TOML override file can
/// reference any action by name. The `serde(rename_all = "kebab-case")` rule
/// turns `ResultsMoveDown` into `"results-move-down"` in TOML — matching the
/// rest of the project's naming convention.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum Action {
    // ─── Results pane: navigation ──────────────────────────────────────
    ResultsMoveDown,
    ResultsMoveUp,
    ResultsMoveLeft,
    ResultsMoveRight,
    ResultsFirstRow,
    ResultsLastRow,

    // ─── Results pane: sort / filter / search ──────────────────────────
    ResultsToggleSort,
    ResultsOpenFilterPrompt,
    ResultsNextMatch,
    ResultsPrevMatch,
    ResultsEscape,

    // ─── Results pane: per-cell / per-row actions ──────────────────────
    ResultsOpenCellPopup,
    ResultsOpenRowDetail,
    ResultsStartCellEdit,
    ResultsYankCell,
    ResultsYankRow,

    // ─── Results pane: cross-statement bundle navigation ───────────────
    ResultsNextStatementLeader,
    ResultsPrevStatementLeader,

    // ─── Row CRUD + Pending changes (L36) ──────────────────────────────
    ResultsAppendRow,
    ResultsDuplicateRow,
    ResultsDeleteRow,
    ResultsCommitPending,
    ResultsDiscardPending,
    ResultsOpenPendingPreview,

    // ─── Metadata tabs (L36) ───────────────────────────────────────────
    MetaTabRecords,
    MetaTabColumns,
    MetaTabConstraints,
    MetaTabForeignKeys,
    MetaTabIndexes,

    // ─── JSON viewer (L36) ─────────────────────────────────────────────
    OpenJsonViewerCell,
    OpenJsonViewerRow,
}

impl Action {
    /// The [`KeyGroup`] that owns this action by default. User overrides may
    /// rebind it in any group, but the default keymap places it here.
    pub const fn default_group(self) -> KeyGroup {
        match self {
            Self::ResultsMoveDown
            | Self::ResultsMoveUp
            | Self::ResultsMoveLeft
            | Self::ResultsMoveRight
            | Self::ResultsFirstRow
            | Self::ResultsLastRow
            | Self::ResultsToggleSort
            | Self::ResultsOpenFilterPrompt
            | Self::ResultsNextMatch
            | Self::ResultsPrevMatch
            | Self::ResultsEscape
            | Self::ResultsOpenCellPopup
            | Self::ResultsOpenRowDetail
            | Self::ResultsStartCellEdit
            | Self::ResultsYankCell
            | Self::ResultsYankRow
            | Self::ResultsNextStatementLeader
            | Self::ResultsPrevStatementLeader
            | Self::ResultsAppendRow
            | Self::ResultsDuplicateRow
            | Self::ResultsDeleteRow
            | Self::ResultsCommitPending
            | Self::ResultsDiscardPending
            | Self::ResultsOpenPendingPreview
            | Self::MetaTabRecords
            | Self::MetaTabColumns
            | Self::MetaTabConstraints
            | Self::MetaTabForeignKeys
            | Self::MetaTabIndexes
            | Self::OpenJsonViewerCell => KeyGroup::Results,
            Self::OpenJsonViewerRow => KeyGroup::RowDetail,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_group_roundtrip() {
        for group in [
            KeyGroup::Global,
            KeyGroup::Editor,
            KeyGroup::Sidebar,
            KeyGroup::Results,
            KeyGroup::RowDetail,
            KeyGroup::CellPopup,
            KeyGroup::JsonViewer,
            KeyGroup::PendingPreview,
        ] {
            assert_eq!(KeyGroup::from_str_opt(group.as_str()), Some(group));
        }
    }

    #[test]
    fn key_group_accepts_snake_alias() {
        assert_eq!(
            KeyGroup::from_str_opt("row_detail"),
            Some(KeyGroup::RowDetail)
        );
        assert_eq!(
            KeyGroup::from_str_opt("pending_preview"),
            Some(KeyGroup::PendingPreview)
        );
    }

    #[test]
    fn action_serde_uses_kebab_case() {
        let s = serde_json::to_string(&Action::ResultsAppendRow).unwrap();
        assert_eq!(s, "\"results-append-row\"");
        let back: Action = serde_json::from_str("\"results-append-row\"").unwrap();
        assert_eq!(back, Action::ResultsAppendRow);
    }

    #[test]
    fn default_group_routes_actions_correctly() {
        assert_eq!(Action::ResultsMoveDown.default_group(), KeyGroup::Results);
        assert_eq!(
            Action::OpenJsonViewerRow.default_group(),
            KeyGroup::RowDetail
        );
        assert_eq!(Action::MetaTabRecords.default_group(), KeyGroup::Results);
    }
}
