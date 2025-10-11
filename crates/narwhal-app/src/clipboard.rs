//! System clipboard abstraction.
//!
//! The [`Clipboard`] trait lets the run-time inject either a real
//! [`ArboardClipboard`] (in the binary) or an [`InMemoryClipboard`] (in
//! headless tests). All `narwhal_app` code that writes to the user's
//! clipboard goes through this trait so that test runs never touch the
//! real desktop session.

use std::sync::Mutex;

/// Write-only handle to the OS clipboard. Reads are intentionally not
/// part of the interface — the app never *consumes* clipboard content,
/// only produces it (yank).
pub trait Clipboard: Send + Sync {
    /// Replace the clipboard contents with `text`. Returns a short error
    /// description on failure; the host app surfaces it via the status
    /// bar.
    fn set_text(&self, text: &str) -> Result<(), String>;
}

/// Backing store for tests. Records the last `set_text` call.
#[derive(Debug, Default)]
pub struct InMemoryClipboard {
    inner: Mutex<Option<String>>,
}

impl InMemoryClipboard {
    pub fn new() -> Self {
        Self::default()
    }

    /// Return the most recently written text, if any.
    pub fn read(&self) -> Option<String> {
        self.inner.lock().ok().and_then(|g| g.clone())
    }
}

impl Clipboard for InMemoryClipboard {
    fn set_text(&self, text: &str) -> Result<(), String> {
        let mut g = self
            .inner
            .lock()
            .map_err(|e| format!("clipboard mutex poisoned: {e}"))?;
        *g = Some(text.to_owned());
        Ok(())
    }
}

/// Production clipboard backed by [`arboard`]. Each `set_text` opens a new
/// arboard handle to dodge issues with long-lived handles on Wayland and
/// X11 displays where the clipboard owner is tied to the window.
#[derive(Debug, Default)]
pub struct ArboardClipboard;

impl ArboardClipboard {
    pub fn new() -> Self {
        Self
    }
}

impl Clipboard for ArboardClipboard {
    fn set_text(&self, text: &str) -> Result<(), String> {
        let mut cb =
            arboard::Clipboard::new().map_err(|e| format!("clipboard unavailable: {e}"))?;
        cb.set_text(text.to_owned())
            .map_err(|e| format!("clipboard write failed: {e}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn in_memory_round_trip() {
        let c = InMemoryClipboard::new();
        assert!(c.read().is_none());
        c.set_text("hello").unwrap();
        assert_eq!(c.read().as_deref(), Some("hello"));
        c.set_text("world").unwrap();
        assert_eq!(c.read().as_deref(), Some("world"));
    }
}
