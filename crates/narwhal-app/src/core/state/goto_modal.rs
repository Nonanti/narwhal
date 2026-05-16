//! `:goto` modal shim — type lives in `narwhal-domain::goto` (Faz 1
//! Madde 3, Adım 6). Re-exported so existing
//! `crate::core::state::goto_modal::{GotoModal, GotoEntry, GotoMatch}`
//! imports keep working.

pub use narwhal_domain::goto::{GotoEntry, GotoMatch, GotoModal};
