//! Keybinding preset integration tests.
//!
//! Verifies that the `VSCode` and `DataGrip` presets layer the expected
//! IDE chords on top of the built-in defaults without breaking the
//! base bindings.

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};
use narwhal_app::DriverRegistry;
use narwhal_app::core::AppCore;
use narwhal_config::{ConnectionsFile, EditorMode, KeyPreset, Settings};

const fn key(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
    KeyEvent {
        code,
        modifiers: mods,
        kind: KeyEventKind::Press,
        state: KeyEventState::NONE,
    }
}

fn core_with_preset(preset: KeyPreset) -> AppCore {
    core_with_preset_and_mode(preset, EditorMode::Vim)
}

fn core_with_preset_and_mode(preset: KeyPreset, mode: EditorMode) -> AppCore {
    let mut core = AppCore::new(
        DriverRegistry::with_defaults(),
        ConnectionsFile::default(),
        None,
    );
    let mut settings = Settings::default();
    settings.keybindings.preset = preset;
    settings.editor.mode = mode;
    core.apply_settings(settings);
    core
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn vscode_preset_routes_ctrl_p_through_goto() {
    let mut core = core_with_preset(KeyPreset::Vscode);
    // No session is open; the goto handler bails with a status
    // message announcing the missing connection. We assert the
    // chord *reached* the handler by inspecting the status text.
    core.handle_key(key(KeyCode::Char('p'), KeyModifiers::CONTROL))
        .await;
    assert!(
        core.status_message().starts_with("goto"),
        "vscode Ctrl-P should reach the goto handler, got {:?}",
        core.status_message(),
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn default_preset_does_not_route_ctrl_p_to_goto() {
    let mut core = core_with_preset(KeyPreset::Default);
    core.handle_key(key(KeyCode::Char('p'), KeyModifiers::CONTROL))
        .await;
    assert!(
        !core.status_message().starts_with("goto"),
        "default Ctrl-P should not trigger the goto handler",
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn datagrip_preset_ctrl_b_focuses_sidebar() {
    let mut core = core_with_preset(KeyPreset::Datagrip);
    core.handle_key(key(KeyCode::Char('b'), KeyModifiers::CONTROL))
        .await;
    assert_eq!(core.focused_pane(), narwhal_tui::Pane::Sidebar);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn intellij_preset_ctrl_b_focuses_sidebar() {
    let mut core = core_with_preset(KeyPreset::Intellij);
    core.handle_key(key(KeyCode::Char('b'), KeyModifiers::CONTROL))
        .await;
    assert_eq!(core.focused_pane(), narwhal_tui::Pane::Sidebar);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn default_preset_ctrl_b_does_not_focus_sidebar() {
    let mut core = core_with_preset(KeyPreset::Default);
    core.handle_key(key(KeyCode::Char('b'), KeyModifiers::CONTROL))
        .await;
    // Default preset leaves Ctrl-B unbound; focus stays on the
    // editor pane which is the default.
    assert_eq!(core.focused_pane(), narwhal_tui::Pane::Editor);
}

// ── B3: preset chords must not steal emacs/basic editor motions ──

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn datagrip_preset_does_not_steal_emacs_ctrl_b() {
    let mut core = core_with_preset_and_mode(KeyPreset::Datagrip, EditorMode::Emacs);
    // Focus starts on the editor pane. In emacs mode, C-b is
    // backward-char; the preset must not hijack it to sidebar.
    assert_eq!(core.focused_pane(), narwhal_tui::Pane::Editor);
    core.handle_key(key(KeyCode::Char('b'), KeyModifiers::CONTROL))
        .await;
    assert_eq!(
        core.focused_pane(),
        narwhal_tui::Pane::Editor,
        "emacs C-b must stay in editor, not jump to sidebar",
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn vscode_preset_does_not_steal_emacs_ctrl_p() {
    let mut core = core_with_preset_and_mode(KeyPreset::Vscode, EditorMode::Emacs);
    assert_eq!(core.focused_pane(), narwhal_tui::Pane::Editor);
    core.handle_key(key(KeyCode::Char('p'), KeyModifiers::CONTROL))
        .await;
    // The goto handler sets a status message starting with "goto";
    // in emacs mode the chord must NOT reach it.
    assert!(
        !core.status_message().starts_with("goto"),
        "emacs C-p must not trigger goto, got {:?}",
        core.status_message(),
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn vscode_preset_active_for_sidebar_focus_even_in_emacs_mode() {
    let mut core = core_with_preset_and_mode(KeyPreset::Vscode, EditorMode::Emacs);
    // Move focus away from the editor so the preset should still fire.
    // (Ctrl-W is intercepted by the emacs short-circuit, so we use the
    // test helper instead.)
    core.set_focus_sidebar_for_test();
    assert_eq!(core.focused_pane(), narwhal_tui::Pane::Sidebar);
    core.handle_key(key(KeyCode::Char('p'), KeyModifiers::CONTROL))
        .await;
    assert!(
        core.status_message().starts_with("goto"),
        "preset Ctrl-P should work when focus is outside editor, got {:?}",
        core.status_message(),
    );
}
