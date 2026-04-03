//! Keybinding preset integration tests.
//!
//! Verifies that the `VSCode` and `DataGrip` presets layer the expected
//! IDE chords on top of the built-in defaults without breaking the
//! base bindings.

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};
use narwhal_app::DriverRegistry;
use narwhal_app::core::AppCore;
use narwhal_config::{ConnectionsFile, KeyPreset, Settings};

const fn key(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
    KeyEvent {
        code,
        modifiers: mods,
        kind: KeyEventKind::Press,
        state: KeyEventState::NONE,
    }
}

fn core_with_preset(preset: KeyPreset) -> AppCore {
    let mut core = AppCore::new(
        DriverRegistry::with_defaults(),
        ConnectionsFile::default(),
        None,
    );
    let mut settings = Settings::default();
    settings.keybindings.preset = preset;
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
