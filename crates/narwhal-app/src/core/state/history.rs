//! History-modal shim — type lives in `narwhal-domain` (Faz 1 Madde 3,
//! Adım 5). Re-exported so existing
//! `crate::core::state::history::HistoryState` imports keep working.

pub use narwhal_domain::history::HistoryState;
