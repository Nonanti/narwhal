//! Write-confirmation modal handler (v1.1 #2).
//!
//! The modal opens from `dispatch_batch` when the active connection
//! declared `confirm_writes = true` and at least one statement is
//! mutating. While open it owns the keyboard; the user must type the
//! accept keyword (`YES`) exactly to resume the held batch, or press
//! Esc to discard it.
//!
//! Keeping this in its own file (rather than inlining into
//! `dispatch.rs`) avoids dragging more concerns into the already
//! 660-line dispatcher and isolates the modal-state plumbing.

use crossterm::event::{KeyCode as CtKey, KeyEvent, KeyModifiers};

use super::{AppCore, PendingConfirm};
use crate::run::RunMode;

impl AppCore {
    pub(super) async fn handle_confirm_key(&mut self, key: KeyEvent) {
        // Esc cancels: drop the modal + the held action, clear the
        // status hint.
        if key.code == CtKey::Esc {
            self.modals.confirm = None;
            self.ui.status.message = "cancelled".into();
            return;
        }

        // Enter checks the accumulated buffer against the accept
        // keyword. If it matches, the held action runs and the modal
        // closes. If it doesn't, we redraw the prompt with a
        // mismatch hint but keep the modal open.
        if key.code == CtKey::Enter {
            let Some(modal) = self.modals.confirm.as_ref() else {
                return;
            };
            if modal.is_satisfied() {
                // Steal the action out before mutating state.
                let action = self.modals.confirm.take().expect("checked above").action;
                self.resume_confirmed(action).await;
            } else {
                let want = modal.accept_keyword.clone();
                self.ui.status.message =
                    format!("type {want} exactly, or Esc to cancel");
            }
            return;
        }

        // Backspace: pop a char from the buffer.
        if key.code == CtKey::Backspace {
            if let Some(modal) = self.modals.confirm.as_mut() {
                modal.buffer.pop();
            }
            return;
        }

        // Ctrl-U: clear the buffer in one stroke (vim-ish reflex).
        if key.code == CtKey::Char('u') && key.modifiers.contains(KeyModifiers::CONTROL) {
            if let Some(modal) = self.modals.confirm.as_mut() {
                modal.buffer.clear();
            }
            return;
        }

        // Any printable character extends the buffer. Modifiers other
        // than Shift are ignored so a stray Alt-Y or Ctrl-Y doesn't
        // count toward the accept keyword.
        if let CtKey::Char(c) = key.code {
            let only_shift = key.modifiers.is_empty()
                || key.modifiers == KeyModifiers::SHIFT;
            if only_shift {
                if let Some(modal) = self.modals.confirm.as_mut() {
                    modal.buffer.push(c);
                }
            }
        }
    }

    /// Resume the action the modal was holding back. Runs in the
    /// same call stack as the key that satisfied the prompt.
    async fn resume_confirmed(&mut self, action: PendingConfirm) {
        match action {
            PendingConfirm::RunMutatingBatch { statements, stream } => {
                let mode = if stream {
                    RunMode::Stream
                } else {
                    RunMode::Execute
                };
                // Bypass the confirm-writes branch on resume —
                // otherwise we'd just re-open the modal. The
                // read-only guard still runs (it never bypasses).
                self.dispatch_batch_with_guards(statements, mode, /* bypass_confirm */ true)
                    .await;
            }
        }
    }
}
