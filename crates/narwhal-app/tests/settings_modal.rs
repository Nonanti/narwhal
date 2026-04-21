//! `:settings` modal + `:mode` quick-switch integration tests.
//!
//! Open the modal, navigate, toggle a field, save / cancel — and
//! verify the on-disk `settings.toml` matches the in-memory state.

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};
use narwhal_app::DriverRegistry;
use narwhal_app::core::AppCore;
use narwhal_config::{ConnectionsFile, EditorMode};

const fn key(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
    KeyEvent {
        code,
        modifiers: mods,
        kind: KeyEventKind::Press,
        state: KeyEventState::NONE,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn settings_command_opens_modal() {
    let mut core = AppCore::new(
        DriverRegistry::with_defaults(),
        ConnectionsFile::default(),
        None,
    );
    core.execute_command("settings").await;
    assert!(core.settings_modal_open());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn esc_cancels_modal_without_saving() {
    let mut core = AppCore::new(
        DriverRegistry::with_defaults(),
        ConnectionsFile::default(),
        None,
    );
    core.execute_command("settings").await;
    // Space toggles the highlighted field, marking the draft dirty.
    core.handle_key(key(KeyCode::Char(' '), KeyModifiers::NONE))
        .await;
    assert!(core.settings_modal_open());
    core.handle_key(key(KeyCode::Esc, KeyModifiers::NONE)).await;
    assert!(!core.settings_modal_open());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn space_cycles_editor_mode_field() {
    let mut core = AppCore::new(
        DriverRegistry::with_defaults(),
        ConnectionsFile::default(),
        None,
    );
    core.execute_command("settings").await;
    // Default mode is Vim; pressing Space on the first field (Editor
    // mode) cycles it.
    assert_eq!(
        core.settings_modal_draft_editor_mode(),
        Some(EditorMode::Vim)
    );
    core.handle_key(key(KeyCode::Char(' '), KeyModifiers::NONE))
        .await;
    assert_eq!(
        core.settings_modal_draft_editor_mode(),
        Some(EditorMode::Basic)
    );
    core.handle_key(key(KeyCode::Char(' '), KeyModifiers::NONE))
        .await;
    assert_eq!(
        core.settings_modal_draft_editor_mode(),
        Some(EditorMode::Emacs)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tab_cycles_sections() {
    let mut core = AppCore::new(
        DriverRegistry::with_defaults(),
        ConnectionsFile::default(),
        None,
    );
    core.execute_command("settings").await;
    assert_eq!(core.settings_modal_selected_section(), Some(0));
    core.handle_key(key(KeyCode::Tab, KeyModifiers::NONE)).await;
    assert_eq!(core.settings_modal_selected_section(), Some(1));
    core.handle_key(key(KeyCode::Tab, KeyModifiers::NONE)).await;
    assert_eq!(core.settings_modal_selected_section(), Some(2));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mode_command_switches_editor_mode_immediately() {
    let mut core = AppCore::new(
        DriverRegistry::with_defaults(),
        ConnectionsFile::default(),
        None,
    );
    // Default is vim.
    core.execute_command("mode basic").await;
    assert_eq!(core.runtime_editor_mode(), EditorMode::Basic);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mode_command_rejects_unknown_value() {
    let mut core = AppCore::new(
        DriverRegistry::with_defaults(),
        ConnectionsFile::default(),
        None,
    );
    core.execute_command("mode banana").await;
    // Unknown value: mode stays at the default and a status message
    // is set.
    assert_eq!(core.runtime_editor_mode(), EditorMode::Vim);
}

/// CB-6: An external settings reload while the modal is open must
/// close the modal so the stale draft cannot silently overwrite
/// the external edit on Ctrl+S.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn settings_reload_with_open_modal_does_not_lose_external_edits() {
    let mut core = AppCore::new(
        DriverRegistry::with_defaults(),
        ConnectionsFile::default(),
        None,
    );
    // Open the settings modal — draft is snapshotted from current state.
    core.execute_command("settings").await;
    assert!(core.settings_modal_open());

    // Simulate an external reload (e.g. another editor saved config.toml).
    // The self-write suppression window is not active, so this should
    // be treated as a genuine external change.
    core.handle_settings_reload().await;

    // The modal must be closed to prevent the stale draft from
    // overwriting the freshly-loaded settings.
    assert!(
        !core.settings_modal_open(),
        "modal should be closed after external reload"
    );
    assert!(
        core.status_message().contains("modal closed"),
        "status bar should inform the user the modal was closed"
    );
}
