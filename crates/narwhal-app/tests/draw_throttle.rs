//! Tests for the streaming-aware redraw scheduler (bug C6).
//!
//! The previous event loop called `self.draw(&mut guard)?` unconditionally
//! on every iteration. The `ResultState::Running.last_render` field
//! promised a 100 ms throttle but was never consulted from the draw
//! decision, so a stream that produced 1 M `RowsAppended` updates per
//! second locked the UI to the same rate and starved F4 / cancel
//! handling.
//!
//! These tests pin the pure `DrawScheduler` that the new event loop
//! consults: force events always draw immediately, stream events
//! coalesce within a 100 ms window, and a deadline tick at the end of
//! the window flushes the pending draw.

use std::time::{Duration, Instant};

use narwhal_app::draw_scheduler::{DrawDecision, DrawScheduler, DrawTrigger, THROTTLE};

#[test]
fn force_event_draws_immediately() {
    let start = Instant::now();
    let mut sched = DrawScheduler::new(start);
    let decision = sched.on_event(DrawTrigger::Force, start);
    assert_eq!(decision, DrawDecision::DrawNow);
}

#[test]
fn stream_event_within_window_is_coalesced() {
    let start = Instant::now();
    let mut sched = DrawScheduler::new(start);
    // First stream event — throttle window is empty, last_draw == start,
    // so the very first stream tick is allowed to draw.
    let d1 = sched.on_event(DrawTrigger::Stream, start);
    assert_eq!(d1, DrawDecision::DrawNow);
    // Subsequent stream events within the same window are coalesced.
    let d2 = sched.on_event(DrawTrigger::Stream, start + Duration::from_millis(10));
    let d3 = sched.on_event(DrawTrigger::Stream, start + Duration::from_millis(50));
    let d4 = sched.on_event(DrawTrigger::Stream, start + Duration::from_millis(99));
    assert_eq!(d2, DrawDecision::Defer);
    assert_eq!(d3, DrawDecision::Defer);
    assert_eq!(d4, DrawDecision::Defer);
    // Once the throttle window has elapsed the next stream event draws.
    let d5 = sched.on_event(DrawTrigger::Stream, start + Duration::from_millis(101));
    assert_eq!(d5, DrawDecision::DrawNow);
}

#[test]
fn deadline_tick_flushes_pending_stream_draw() {
    let start = Instant::now();
    let mut sched = DrawScheduler::new(start);
    // Burn the first stream draw, then defer.
    sched.on_event(DrawTrigger::Stream, start);
    sched.on_event(DrawTrigger::Stream, start + Duration::from_millis(10));
    // Deadline tick AT the boundary flushes the pending draw.
    let d = sched.on_tick(start + Duration::from_millis(100));
    assert_eq!(d, DrawDecision::DrawNow);
    // After the flush, a fresh tick with no new events is a no-op.
    let d2 = sched.on_tick(start + Duration::from_millis(200));
    assert_eq!(d2, DrawDecision::Defer);
}

#[test]
fn deadline_reports_when_stream_pending() {
    let start = Instant::now();
    let mut sched = DrawScheduler::new(start);
    // Nothing pending → no deadline.
    assert!(sched.deadline().is_none());
    // First stream event drew; no defer yet.
    sched.on_event(DrawTrigger::Stream, start);
    assert!(sched.deadline().is_none());
    // Second stream event in the window is deferred — deadline now set.
    sched.on_event(DrawTrigger::Stream, start + Duration::from_millis(20));
    let dl = sched
        .deadline()
        .expect("deadline must be set after a defer");
    assert_eq!(dl, start + THROTTLE);
}

#[test]
fn thousand_stream_events_in_one_window_emit_one_draw() {
    let start = Instant::now();
    let mut sched = DrawScheduler::new(start);
    let mut draws = 0usize;
    for i in 0..1000 {
        // Spread events across the first 50 ms — all in the same window.
        let t = start + Duration::from_micros(50 * i);
        if matches!(
            sched.on_event(DrawTrigger::Stream, t),
            DrawDecision::DrawNow
        ) {
            draws += 1;
        }
    }
    assert_eq!(
        draws, 1,
        "expected the first stream event to draw and the rest to coalesce, got {draws}"
    );
}

#[test]
fn force_event_supersedes_pending_stream_defer() {
    let start = Instant::now();
    let mut sched = DrawScheduler::new(start);
    sched.on_event(DrawTrigger::Stream, start); // first draws
    sched.on_event(DrawTrigger::Stream, start + Duration::from_millis(10)); // defers
    // A key event mid-window must draw immediately and clear the
    // pending defer.
    let d = sched.on_event(DrawTrigger::Force, start + Duration::from_millis(20));
    assert_eq!(d, DrawDecision::DrawNow);
    assert!(
        sched.deadline().is_none(),
        "force draw should clear pending"
    );
}
