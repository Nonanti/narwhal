//! Integration tests for `:`-prompt tab-completion.
//!
//! These tests drive the prompt through `handle_key`, typing `:` to enter
//! command mode, filling in characters and pressing Tab, then asserting
//! the resulting command-buffer contents.

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};
use narwhal_app::core::AppCore;
use narwhal_app::DriverRegistry;
use narwhal_config::ConnectionsFile;
use narwhal_core::{ConnectionConfig, ConnectionParams};
use narwhal_vim::Mode;
use tempfile::TempDir;
use uuid::Uuid;

const fn key(code: KeyCode) -> KeyEvent {
    KeyEvent {
        code,
        modifiers: KeyModifiers::NONE,
        kind: KeyEventKind::Press,
        state: KeyEventState::NONE,
    }
}

fn core_with_connections(names: &[&str]) -> AppCore {
    let registry = DriverRegistry::with_defaults();
    let connections = ConnectionsFile {
        logical_relations: Vec::new(),
        connections: names
            .iter()
            .map(|name| ConnectionConfig {
                id: Uuid::new_v4(),
                name: (*name).to_owned(),
                driver: "sqlite".into(),
                params: ConnectionParams::with(|p| {
                    p.path = Some(":memory:".into());
                }),
            })
            .collect(),
    };
    AppCore::new(registry, connections, None)
}

fn empty_core() -> AppCore {
    let registry = DriverRegistry::with_defaults();
    let connections = ConnectionsFile {
        logical_relations: Vec::new(),
        connections: Vec::new(),
    };
    AppCore::new(registry, connections, None)
}

/// Type a string into the command prompt. Assumes the core is already in
/// command mode (caller should press `:` first).
async fn type_prompt(core: &mut AppCore, text: &str) {
    for ch in text.chars() {
        core.handle_key(key(KeyCode::Char(ch))).await;
    }
}

// Test 1: `:open <prefix>` with a unique match → completes inline

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn open_unique_completes_inline() {
    let mut core = core_with_connections(&["smoke"]);

    // Enter command mode and type "open sm"
    core.handle_key(key(KeyCode::Char(':'))).await;
    assert_eq!(core.mode(), Mode::Command);
    type_prompt(&mut core, "open sm").await;

    // Press Tab
    core.handle_key(key(KeyCode::Tab)).await;

    // The buffer should now contain "open smoke"
    assert_eq!(core.command_buffer(), "open smoke");
}

// Test 2: `:open <prefix>` with multiple matches → inserts LCP, lists

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn open_multiple_inserts_lcp() {
    let mut core = core_with_connections(&["smoke", "smolder"]);

    core.handle_key(key(KeyCode::Char(':'))).await;
    type_prompt(&mut core, "open sm").await;

    core.handle_key(key(KeyCode::Tab)).await;

    // The longest common prefix of "smoke" and "smolder" is "smo"
    assert_eq!(core.command_buffer(), "open smo");
    // Status bar lists both candidates
    let status = core.status_message();
    assert!(
        status.contains("smoke") && status.contains("smolder"),
        "expected both names in status, got: {status}"
    );
}

// Test 3: `:help <prefix>` completes a built-in command name

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn help_completes_builtin() {
    let mut core = empty_core();

    core.handle_key(key(KeyCode::Char(':'))).await;
    type_prompt(&mut core, "help op").await;

    core.handle_key(key(KeyCode::Tab)).await;

    assert_eq!(core.command_buffer(), "help open");
}

// Test 4: `:help <prefix>` completes a plugin command name

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn help_completes_plugin() {
    let dir = TempDir::new().unwrap();
    let script = dir.path().join("rc.lua");
    std::fs::write(
        &script,
        r#"
narwhal.register_command("rc", "rc test command", function(arg)
    return "rc got: " .. arg
end)
"#,
    )
    .unwrap();

    let mut core = empty_core();
    core.execute_command(&format!("plug-load {}", script.display()))
        .await;

    core.handle_key(key(KeyCode::Char(':'))).await;
    type_prompt(&mut core, "help r").await;

    core.handle_key(key(KeyCode::Tab)).await;

    // The buffer should contain "help r" plus the longest common prefix
    // of all names starting with "r". At minimum "rc" should be among
    // the candidates shown in the status message.
    let status = core.status_message();
    assert!(
        status.contains("rc"),
        "expected 'rc' in completion candidates, got status: {status}"
    );
}

// Test 5: `:export <prefix>` completes the format

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn export_completes_format() {
    let mut core = empty_core();

    core.handle_key(key(KeyCode::Char(':'))).await;
    type_prompt(&mut core, "export c").await;

    core.handle_key(key(KeyCode::Tab)).await;

    assert_eq!(core.command_buffer(), "export csv");
}

// Test 6: Unknown command head → Tab is a no-op

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unknown_head_is_noop() {
    let mut core = empty_core();

    core.handle_key(key(KeyCode::Char(':'))).await;
    type_prompt(&mut core, "zz a").await;

    let before = core.command_buffer().to_owned();
    core.handle_key(key(KeyCode::Tab)).await;

    assert_eq!(core.command_buffer(), before);
}

// Test 7: Bare `:` (empty buffer) → no completion

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bare_colon_no_completion() {
    let mut core = core_with_connections(&["smoke"]);

    core.handle_key(key(KeyCode::Char(':'))).await;
    // Don't type anything — buffer is empty.

    let before = core.command_buffer().to_owned();
    core.handle_key(key(KeyCode::Tab)).await;

    assert_eq!(core.command_buffer(), before);
}
