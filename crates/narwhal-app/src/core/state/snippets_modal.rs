//! `:snippets` modal shim — type lives in `narwhal-domain::snippets`
//! (Faz 1 Madde 3, Adım 6). Re-exported so existing
//! `crate::core::state::snippets_modal::SnippetsModal` imports keep
//! working.

pub use narwhal_domain::snippets::SnippetsModal;
