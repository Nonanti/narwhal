//! Terminal user interface built on top of `ratatui`.

#![forbid(unsafe_code)]

pub mod constants;
pub mod input;
pub mod layout;
pub mod theme;
pub mod widgets;

pub use input::translate_key_event;
pub use layout::{render_root, LayoutRegions, Pane, RootLayout, StatusBarView};
pub use theme::Theme;
// `widgets` already re-exports each public type from its child modules;
// glob-import here so there's a single source of truth instead of the
// duplicated explicit list this used to maintain (L26).
pub use widgets::*;
