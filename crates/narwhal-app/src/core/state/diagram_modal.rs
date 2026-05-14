//! Diagram modal state.
//!
//! Carved out of `state/result.rs` so the rest of the result-pane
//! state can move down to `narwhal-domain` without dragging the
//! `narwhal-diagram` dependency along. Diagram already depends on
//! the domain crate (it re-exports `narwhal_domain::QualifiedName`),
//! so anything that names `DiagramModel` / `IconSet` / `ImpactTree`
//! has to live above it in the dependency graph — here in the app
//! layer is the natural home.

use narwhal_diagram::{DiagramModel, IconSet, ImpactTree, QualifiedName};

/// Which view the diagram modal is rendering.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiagramMode {
    /// Focused: centre table with its columns + 1-hop FK neighbours
    /// listed below.
    Focused,
    /// Impact: reverse-FK tree rooted at the centre table.
    Impact,
}

/// In-flight diagram modal. Owns the full schema model (described
/// once at open time) plus the navigational cursor; the widget re-renders
/// from this state every frame.
#[derive(Debug, Clone)]
pub struct DiagramModalState {
    pub mode: DiagramMode,
    /// Full diagram for the active schema(s). Cached so re-centering
    /// (Enter on a neighbour) is instant — no extra round-trips.
    pub model: DiagramModel,
    /// Currently-centered table.
    pub center: QualifiedName,
    /// Reverse-FK closure rooted at `center`. Recomputed on every
    /// centre change (cheap; pure walk over the cached model).
    pub impact: ImpactTree,
    /// Selection index inside the navigable neighbours list
    /// (outbound first, then inbound). Used by Tab / Enter in Focused mode.
    pub selected: usize,
    /// Vertical scroll for the body when content exceeds the modal.
    pub scroll: u16,
    /// Glyph set resolved from `[diagram].icons`. Stored on the modal so
    /// a runtime config reload would only take effect on the *next* open
    /// — keeping every render in this session visually consistent.
    pub icons: IconSet,
}
