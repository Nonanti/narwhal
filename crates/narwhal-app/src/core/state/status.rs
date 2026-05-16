//! Status-bar shim — type lives in `narwhal-domain` (Faz 1 Madde 3,
//! Adım 5). Re-exported so existing
//! `crate::core::state::status::{StatusBar, Notification}` imports
//! keep working.

pub use narwhal_domain::status::{Notification, StatusBar};
