//! In-app settings modal: open / commit / cancel + the `:mode`
//! quick-switch command.
//!
//! Render lives in `narwhal_tui::widgets::settings_modal`; key
//! handling lives in `editor_dispatch::settings_modal`. This file
//! ties them to `AppCore` and owns the on-disk persistence path.

use narwhal_config::{EditorMode, Settings};

use crate::core::AppCore;
use crate::core::state::SettingsModal;

impl AppCore {
    /// Open the `:settings` modal with the current settings as the
    /// initial draft. Closing the modal without saving restores
    /// the original payload from the `original` snapshot.
    pub async fn open_settings_modal(&mut self) {
        let current = self.current_settings_snapshot();
        self.modals.settings = Some(SettingsModal::new(current));
        self.ui.status.message = "settings: Tab cycles section, Space toggles, Ctrl+S saves".into();
    }

    /// Persist the modal draft to `settings.toml` and re-apply it
    /// to the running app. Closes the modal on success, leaves it
    /// open with a status-bar error on failure.
    pub async fn commit_settings_modal(&mut self) {
        let Some(modal) = self.modals.settings.as_ref() else {
            return;
        };
        let draft = modal.draft.clone();
        match self.persist_settings(&draft).await {
            Ok(()) => {
                self.modals.settings = None;
                self.apply_settings(draft);
                self.ui.status.message = "settings saved".into();
            }
            Err(e) => {
                self.ui.status.message = format!("settings save failed: {e}");
            }
        }
    }

    /// Discard the in-progress draft without touching disk.
    pub async fn cancel_settings_modal(&mut self) {
        if let Some(modal) = self.modals.settings.take() {
            if modal.dirty {
                self.ui.status.message = "settings: changes discarded".into();
            } else {
                self.ui.status.message = "ready".into();
            }
        }
    }

    /// `:mode vim|basic|emacs` — switch the editor input model on
    /// the fly. Equivalent to opening the modal, flipping the mode
    /// field, and saving — but bypasses the UI.
    pub async fn switch_editor_mode_command(&mut self, arg: &str) {
        let target = match arg.trim().to_ascii_lowercase().as_str() {
            "vim" => EditorMode::Vim,
            "basic" => EditorMode::Basic,
            "emacs" => EditorMode::Emacs,
            other => {
                self.ui.status.message = format!("mode: expected vim|basic|emacs, got '{other}'");
                return;
            }
        };
        let mut next = self.current_settings_snapshot();
        next.editor.mode = target;
        if let Err(e) = self.persist_settings(&next).await {
            self.ui.status.message = format!("mode: save failed: {e}");
            return;
        }
        self.apply_settings(next);
        self.ui.status.message = format!(
            "editor mode → {}",
            match target {
                EditorMode::Vim => "vim",
                EditorMode::Basic => "basic",
                EditorMode::Emacs => "emacs",
                _ => "vim",
            }
        );
    }

    /// Snapshot the live runtime state back into a Settings payload.
    /// Used as the draft baseline when the modal opens or `:mode`
    /// is invoked. Only the fields the runtime actually owns get
    /// re-derived; the rest fall back to disk-resident defaults via
    /// `Settings::load`.
    fn current_settings_snapshot(&self) -> Settings {
        let mut s = self.load_settings_from_disk().unwrap_or_default();
        s.editor.mode = self.ui.editor_mode;
        s.editor.mouse = self.ui.mouse_mode;
        s.editor.show_mode_indicator = self.ui.show_mode_indicator;
        // Theme is not reverse-mapped from the live palette because
        // the runtime stores the resolved colours, not the enum. We
        // trust the disk copy as the source of truth for it.
        s
    }

    /// Try to load `settings.toml` from the standard XDG path.
    fn load_settings_from_disk(&self) -> Option<Settings> {
        let paths = narwhal_config::ConfigPaths::discover().ok()?;
        Settings::load(&paths.settings_file()).ok()
    }

    /// Atomically write `settings` to `settings.toml`. Bumps the
    /// `last_self_settings_write` timestamp so the live-reload
    /// watcher can suppress the echo of our own write.
    async fn persist_settings(&mut self, settings: &Settings) -> Result<(), String> {
        let paths =
            narwhal_config::ConfigPaths::discover().map_err(|e| format!("config paths: {e}"))?;
        settings
            .save(&paths.settings_file())
            .map_err(|e| format!("{e}"))?;
        self.last_self_settings_write = Some(std::time::Instant::now());
        Ok(())
    }

    /// Handle a `notify`-driven settings reload trigger. Re-reads
    /// `settings.toml` and re-applies it. Suppresses the echo of a
    /// modal-save we just performed (a 750 ms window after a
    /// `persist_settings` call), so the modal's apply-then-watcher
    /// chain doesn't double-apply the same payload.
    pub async fn handle_settings_reload(&mut self) {
        if let Some(t) = self.last_self_settings_write {
            if t.elapsed() < std::time::Duration::from_millis(750) {
                return;
            }
        }
        let Some(settings) = self.load_settings_from_disk() else {
            return;
        };
        self.apply_settings(settings);
        self.ui.status.notify(
            "settings reloaded from disk",
            std::time::Duration::from_secs(3),
        );
    }
}
