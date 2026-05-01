//! CB-11: vim state must reset to Normal on tab switch/open/close.
//!
//! `self.ui.vim` is a single global instance shared across all tabs.
//! Without a reset, transient modes (Visual, Command, Insert,
//! `OperatorPending`) leak into the next tab — causing phantom
//! selections, misrouted command buffers, and unexpected edits.

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};
use narwhal_app::DriverRegistry;
use narwhal_app::core::AppCore;
use narwhal_config::ConnectionsFile;
use narwhal_core::{ConnectionConfig, ConnectionParams};
use narwhal_vim::Mode;
use uuid::Uuid;

fn fixture() -> (DriverRegistry, ConnectionsFile) {
    let registry = DriverRegistry::with_defaults();
    let connections = ConnectionsFile {
        schema_version: None,
        logical_relations: Vec::new(),
        connections: vec![ConnectionConfig {
            id: Uuid::nil(),
            name: "vim-tab".into(),
            driver: "sqlite".into(),
            params: ConnectionParams::with(|p| {
                p.path = Some(":memory:".into());
            }),
        }],
    };
    (registry, connections)
}

const fn key(code: KeyCode) -> KeyEvent {
    KeyEvent {
        code,
        modifiers: KeyModifiers::NONE,
        kind: KeyEventKind::Press,
        state: KeyEventState::NONE,
    }
}

/// Enter Visual mode on tab 1, switch to tab 2 — vim must be Normal.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tab_switch_resets_visual_mode() {
    let (registry, connections) = fixture();
    let mut core = AppCore::new(registry, connections, None);

    // Open a second tab so cycle_tab has somewhere to go.
    core.execute_command("new").await;
    assert_eq!(core.tabs().len(), 2);

    // Go back to tab 0 and enter Visual mode.
    core.execute_command("tabprev").await;
    assert_eq!(core.active_tab(), 0);
    core.handle_key(key(KeyCode::Char('v'))).await;
    assert_eq!(core.mode(), Mode::Visual);

    // Switch to tab 1 — mode must reset.
    core.execute_command("tabnext").await;
    assert_eq!(core.active_tab(), 1);
    assert_eq!(
        core.mode(),
        Mode::Normal,
        "Visual mode leaked across tab switch"
    );
}

/// Enter Command mode (`:`) on tab 1, switch to tab 2 — vim must be
/// Normal and the command buffer must be empty.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tab_switch_resets_command_mode() {
    let (registry, connections) = fixture();
    let mut core = AppCore::new(registry, connections, None);

    core.execute_command("new").await;
    core.execute_command("tabprev").await;

    // Enter Command mode and type a partial command.
    core.handle_key(key(KeyCode::Char(':'))).await;
    assert_eq!(core.mode(), Mode::Command);
    core.handle_key(key(KeyCode::Char('w'))).await;
    assert!(!core.command_buffer().is_empty());

    // Switch to tab 1 — mode and buffer must reset.
    core.execute_command("tabnext").await;
    assert_eq!(
        core.mode(),
        Mode::Normal,
        "Command mode leaked across tab switch"
    );
    assert!(
        core.command_buffer().is_empty(),
        "command buffer leaked across tab switch"
    );
}

/// Enter Insert mode on tab 1, switch to tab 2 — vim must be Normal.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tab_switch_resets_insert_mode() {
    let (registry, connections) = fixture();
    let mut core = AppCore::new(registry, connections, None);

    core.execute_command("new").await;
    core.execute_command("tabprev").await;

    // Enter Insert mode.
    core.handle_key(key(KeyCode::Char('i'))).await;
    assert_eq!(core.mode(), Mode::Insert);

    // Switch to tab 1 — mode must reset.
    core.execute_command("tabnext").await;
    assert_eq!(
        core.mode(),
        Mode::Normal,
        "Insert mode leaked across tab switch"
    );
}

/// Opening a new tab resets vim to Normal.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn new_tab_resets_vim_mode() {
    let (registry, connections) = fixture();
    let mut core = AppCore::new(registry, connections, None);

    // Enter Visual mode.
    core.handle_key(key(KeyCode::Char('v'))).await;
    assert_eq!(core.mode(), Mode::Visual);

    // Open new tab — vim must be Normal on the new tab.
    core.execute_command("new").await;
    assert_eq!(core.tabs().len(), 2);
    assert_eq!(core.mode(), Mode::Normal, "Visual mode leaked into new tab");
}

/// Closing a tab that was in Visual mode must not leave the survivor
/// in Visual.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn close_tab_resets_vim_mode() {
    let (registry, connections) = fixture();
    let mut core = AppCore::new(registry, connections, None);

    // Open a second tab, enter Visual on it, then close it.
    core.execute_command("new").await;
    core.handle_key(key(KeyCode::Char('v'))).await;
    assert_eq!(core.mode(), Mode::Visual);

    core.execute_command("tabclose").await;
    assert_eq!(core.tabs().len(), 1);
    assert_eq!(
        core.mode(),
        Mode::Normal,
        "Visual mode from closed tab leaked into survivor"
    );
}
