//! Terminal user interface built on top of `ratatui`.

#![forbid(unsafe_code)]

pub mod input;
pub mod layout;
pub mod theme;
pub mod widgets;

pub use input::translate_key_event;
pub use layout::{render_root, RootLayout};
pub use theme::Theme;
