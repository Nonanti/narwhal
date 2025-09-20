//! narwhal-tui — Ratatui UI layer.
//!
//! Layout (initial):
//! ```text
//! ┌──────────────┬──────────────────────────────────────────┐
//! │  Sidebar     │  Editor / Result tabs                    │
//! │  (schemas    │                                          │
//! │   & tables)  │                                          │
//! │              │                                          │
//! └──────────────┴──────────────────────────────────────────┘
//!   NOR | conn: prod-readonly | db: postgres | 12 rows · 42ms
//! ```

pub mod input;
pub mod layout;
pub mod theme;
pub mod widgets;

pub use input::translate_key_event;
pub use layout::{render_root, RootLayout};
pub use theme::Theme;
