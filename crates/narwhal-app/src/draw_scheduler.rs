//! Streaming-aware redraw scheduler used by the terminal event loop.
//!
//! Splits redraws into two classes:
//!
//! - **Force** triggers (key, mouse, resize, non-streaming run updates)
//!   draw immediately.
//! - **Stream** triggers (`RunUpdate::RowsAppended`) are coalesced into
//!   at most one draw per [`THROTTLE`] window. A burst of N stream
//!   updates inside the same window emits exactly one initial draw and
//!   one trailing draw at the deadline.
//!
//! The scheduler is pure: the event loop feeds it [`DrawTrigger`]s and
//! `Instant`s, the scheduler hands back a [`DrawDecision`]. No clock or
//! channel access — everything is testable in isolation (bug C6).
//!
//! ```text
//!   first stream event  ── DrawNow ────────────────────────┐
//!   in-window stream    ── Defer ── deadline set           │
//!   in-window stream    ── Defer                           │
//!   tick at deadline    ── DrawNow ── deadline cleared ────┘
//!   force event mid-w.  ── DrawNow ── deadline cleared
//! ```
//!
//! Anything that draws (force or deadline flush) resets `last_draw`, so
//! the next throttle window starts from that moment.

use std::time::{Duration, Instant};

/// Maximum redraw rate for streaming `RowsAppended` events.
pub const THROTTLE: Duration = Duration::from_millis(100);

/// Why the event loop is considering a redraw.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum DrawTrigger {
    /// User input (key, mouse, resize) or a non-streaming run update
    /// (StatementStarted, Failed, AllDone, SchemaRefresh, …). Always
    /// draws immediately.
    Force,
    /// A `RunUpdate::RowsAppended` carrying a fresh batch of rows.
    /// Coalesced inside the throttle window.
    Stream,
}

/// What the scheduler decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum DrawDecision {
    /// Draw now and reset `last_draw`.
    DrawNow,
    /// Skip this iteration; the deadline timer will flush later.
    Defer,
}

/// Pure decision engine — owns no clock or channel.
#[derive(Debug, Default)]
pub struct DrawScheduler {
    /// When we last drew. `None` means "never drawn" — the first event
    /// of any kind passes the throttle check.
    last_draw: Option<Instant>,
    /// True after a stream event inside the throttle window has been
    /// deferred — the deadline timer must wake and flush it.
    pending: bool,
}

impl DrawScheduler {
    /// Construct a fresh scheduler. The first event of any kind will
    /// draw immediately; the throttle window starts from that draw.
    pub fn new(_initial: Instant) -> Self {
        Self {
            last_draw: None,
            pending: false,
        }
    }

    /// Feed an event into the scheduler. Returns whether the caller
    /// should draw right now.
    pub fn on_event(&mut self, trigger: DrawTrigger, now: Instant) -> DrawDecision {
        match trigger {
            DrawTrigger::Force => self.flush(now),
            DrawTrigger::Stream => match self.last_draw {
                None => self.flush(now),
                Some(prev) if now.duration_since(prev) >= THROTTLE => self.flush(now),
                Some(_) => {
                    self.pending = true;
                    DrawDecision::Defer
                }
            },
        }
    }

    /// Called when the deadline timer fires. Flushes any pending defer.
    pub fn on_tick(&mut self, now: Instant) -> DrawDecision {
        if self.pending {
            self.flush(now)
        } else {
            DrawDecision::Defer
        }
    }

    /// If a flush is pending, when the deadline timer should wake.
    /// `None` means there is nothing to flush.
    pub fn deadline(&self) -> Option<Instant> {
        match (self.pending, self.last_draw) {
            (true, Some(prev)) => Some(prev + THROTTLE),
            _ => None,
        }
    }

    fn flush(&mut self, now: Instant) -> DrawDecision {
        self.last_draw = Some(now);
        self.pending = false;
        DrawDecision::DrawNow
    }
}
