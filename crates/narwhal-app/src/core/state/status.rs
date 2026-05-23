//! Three-slot status bar state shared by the renderer.

#[derive(Debug, Default, Clone)]
pub struct StatusBar {
    /// Center slot — set once on connect, cleared on disconnect.
    pub connection: Option<String>,
    /// Right slot — last transient message.
    pub message: String,
    /// Optional fourth slot — open transaction's isolation level.
    pub transaction: Option<String>,
}
