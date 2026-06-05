//! Three-slot status bar state shared by the renderer.

use std::time::{Duration, Instant};

#[derive(Debug, Default, Clone)]
pub struct StatusBar {
    /// Center slot — set once on connect, cleared on disconnect.
    pub connection: Option<String>,
    /// Right slot — last transient message.
    pub message: String,
    /// Optional fourth slot — open transaction's isolation level.
    pub transaction: Option<String>,
    /// MR-M3: sticky notification slot. Set via [`Self::notify`]; the
    /// renderer prefers this over [`Self::message`] until it expires,
    /// so a one-shot event (e.g. "multi-line paste collapsed secondary
    /// cursors") survives the next keystroke instead of being
    /// overwritten in the same frame.
    pub notification: Option<Notification>,
}

/// A transient toast message with its own deadline.
#[derive(Debug, Clone)]
pub struct Notification {
    pub text: String,
    pub expires_at: Instant,
}

impl StatusBar {
    /// Post a notification that should stay visible for `ttl`,
    /// regardless of other `status.message =` writes that happen in
    /// the same frame.
    ///
    /// **TTL semantics (R3-M3):** the cap is enforced *the next
    /// time the renderer is invoked* after `expires_at`. The draw
    /// scheduler ([`crate::draw_scheduler`]) is event-driven — it
    /// fires on input, mouse, resize, and stream updates, but does
    /// not run a wall-clock tick. So with no other activity the
    /// notification can linger on screen past `ttl` until the next
    /// triggering event. For the only current caller (multi-line
    /// paste warning, `Duration::from_secs(3)`) this is acceptable:
    /// the very next keystroke clears it. Callers that need a hard
    /// deadline should also wake the draw scheduler at
    /// `expires_at`.
    pub fn notify<S: Into<String>>(&mut self, text: S, ttl: Duration) {
        self.notification = Some(Notification {
            text: text.into(),
            expires_at: Instant::now() + ttl,
        });
    }

    /// R3-N4: read-only peek used by the render path. Returns the
    /// active notification text, or `None` once `expires_at` has
    /// passed. Does **not** mutate the slot — separating peek from
    /// expiry keeps the render call genuinely pure (no implicit
    /// state change) and lets two renderers cooperate without
    /// borrow conflicts.
    pub fn peek_notification(&self) -> Option<&str> {
        let n = self.notification.as_ref()?;
        if Instant::now() < n.expires_at {
            Some(n.text.as_str())
        } else {
            None
        }
    }

    /// R3-N4: companion to [`Self::peek_notification`]; drops an
    /// expired notification so the slot can be reused. Called from
    /// the event loop once per turn (input / stream tick), keeping
    /// the render path free of mutation.
    pub fn tick_expired(&mut self) {
        if self
            .notification
            .as_ref()
            .is_some_and(|n| Instant::now() >= n.expires_at)
        {
            self.notification = None;
        }
    }

    /// Legacy shim kept for the inline render-time fallback path:
    /// peeks and clears in one go. Prefer [`Self::peek_notification`]
    /// + [`Self::tick_expired`] for new callers — they are easier
    /// to reason about because the render is read-only.
    #[doc(hidden)]
    pub fn current_notification(&mut self) -> Option<&str> {
        self.tick_expired();
        self.notification.as_ref().map(|n| n.text.as_str())
    }
}
