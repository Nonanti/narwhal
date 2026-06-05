//! Process-lifecycle and async-bridge state.
//!
//! This sub-state owns the moving parts of the event loop: the
//! shutdown flag, the in-flight run/refresh tracking, and the
//! channels that ferry background results back to the UI thread.
//! Bundled together so the lifecycle invariants are obvious in one
//! place:
//!
//! - `should_quit` is set by the global Esc/Ctrl-C handler and read
//!   by the top-level `run` loop. Nothing else writes it.
//! - `running` is true while a query batch is in flight. The status
//!   bar reads it to render the spinner; cancel logic sets the
//!   slot at `cancel_slot` and the worker honours it.
//! - `run_tx` / `meta_tx` are the *sender* halves of the two
//!   channels the UI hands to background tasks. The matching
//!   receivers stay on the event loop because draining them needs
//!   mutable access to `AppCore`. (Receivers therefore live next to
//!   `ProcessState` in `AppCore`, not inside it.)
//! - `refresh_task` is the abort-handle for the current debounced
//!   schema-refresh timer; replaced on each scheduling call.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use tokio::sync::mpsc;

use crate::meta::MetaUpdate;
use crate::run::{ActiveCancel, RunUpdate};

/// Lifecycle + async-bridge state. Holds only `Send` data; safe to
/// share with background tasks via the channel senders.
pub struct ProcessState {
    /// Set to `true` by the global quit handler (`:quit`, Ctrl-D on
    /// an empty editor, Ctrl-C with no in-flight job). The top-level
    /// `run` loop polls it on every tick and exits cleanly when set.
    pub should_quit: bool,
    /// True while a query batch is currently running. Drives the
    /// status-line spinner and gates a few keybindings (`R` to
    /// re-run, `:run` while another run is live).
    pub running: bool,
    /// Tab index that owns the in-flight run, captured at
    /// `dispatch_batch` time so a mid-run tab switch cannot
    /// scribble results into the wrong tab. Cleared to `None` on
    /// `AllDone`. (Bug K1-A fix.)
    pub run_tab: Option<usize>,
    /// Cancellation handle published by the active run. The
    /// dispatcher swaps a fresh `CancellationToken` in on every
    /// new batch; Ctrl-C calls `cancel()` on the current token.
    pub cancel_slot: ActiveCancel,
    /// One-shot warning carried over from a plugin (transform or
    /// command hook) so that the final 'done · N statement(s)'
    /// message does not silently overwrite it. Cleared after it
    /// bubbles up to the status bar.
    pub plugin_warning: Option<String>,
    /// Abort handle for the in-flight debounced schema-refresh
    /// task. Replaced on every `schedule_schema_refresh` call so
    /// the new timer cancels the old one.
    pub refresh_task: Option<tokio::task::AbortHandle>,
    /// Shared flag set by `schedule_schema_refresh` and consumed
    /// by the debounce timer task to know whether a refresh is
    /// still pending. Atomic so the timer can read it without
    /// holding any lock.
    pub refresh_pending: Arc<AtomicBool>,
    /// Sender half of the run-update channel. Cloned freely into
    /// background workers; each query batch ships statement
    /// completions, row chunks, and the final `AllDone` through it.
    pub run_tx: mpsc::Sender<RunUpdate>,
    /// Sender half of the meta-update channel. Cloned into
    /// metadata workers (`OpenSession`, `RefreshSchemas`,
    /// `TestConnection`, `InjectDdlReady`, `CredentialReady`,
    /// `ForgetCompleted`). Separated from the run channel so meta
    /// ops do not interfere with query execution state.
    pub meta_tx: mpsc::Sender<MetaUpdate>,
}

impl ProcessState {
    /// Construct a fresh `ProcessState` wired to the supplied channel
    /// senders. The receivers stay outside this struct because
    /// draining them needs mutable access to `AppCore` (the handlers
    /// mutate UI/session/modal state, not just process state).
    pub fn new(
        run_tx: mpsc::Sender<RunUpdate>,
        meta_tx: mpsc::Sender<MetaUpdate>,
        cancel_slot: ActiveCancel,
    ) -> Self {
        Self {
            should_quit: false,
            running: false,
            run_tab: None,
            cancel_slot,
            plugin_warning: None,
            refresh_task: None,
            refresh_pending: Arc::new(AtomicBool::new(false)),
            run_tx,
            meta_tx,
        }
    }
}
