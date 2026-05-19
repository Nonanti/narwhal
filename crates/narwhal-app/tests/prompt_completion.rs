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

fn key(code: KeyCode) -> KeyEvent {
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
        connections: names
            .iter()
            .map(|name| ConnectionConfig {
                id: Uuid::new_v4(),
                name: (*name).to_owned(),
                driver: "sqlite".into(),
                params: ConnectionParams {
                    path: Some(":memory:".into()),
                    ..Default::default()
                },
            })
            .collect(),
    };
    AppCore::new(registry, connections, None)
}

fn empty_core() -> AppCore {
    let registry = DriverRegistry::with_defaults();
    let connections = ConnectionsFile {
        connections: Vec::new(),
    };
    AppCore::new(registry, connections, None)
}

/// Type a string into the command prompt. Assumes the core is already in
/// command mode (caller should press `:` first).
fn type_prompt(core: &mut AppCore, text: &str) {
    for ch in text.chars() {
        core.handle_key(key(KeyCode::Char(ch)));
    }
}

// ---------------------------------------------------------------------------
// Test 1: `:open <prefix>` with a unique match → completes inline
// ---------------------------------------------------------------------------

#[test]
fn open_unique_completes_inline() {
    let mut core = core_with_connections(&["smoke"]);

    // Enter command mode and type "open sm"
    core.handle_key(key(KeyCode::Char(':')));
    assert_eq!(core.mode(), Mode::Command);
    type_prompt(&mut core, "open sm");

    // Press Tab
    core.handle_key(key(KeyCode::Tab));

    // The buffer should now contain "open smoke"
    assert_eq!(core.command_buffer(), "open smoke");
}

// ---------------------------------------------------------------------------
// Test 2: `:open <prefix>` with multiple matches → inserts LCP, lists
// ---------------------------------------------------------------------------

#[test]
fn open_multiple_inserts_lcp() {
    let mut core = core_with_connections(&["smoke", "smolder"]);

    core.handle_key(key(KeyCode::Char(':')));
    type_prompt(&mut core, "open sm");

    core.handle_key(key(KeyCode::Tab));

    // The longest common prefix of "smoke" and "smolder" is "smo"
    assert_eq!(core.command_buffer(), "open smo");
    // Status bar lists both candidates
    let status = core.status_message();
    assert!(
        status.contains("smoke") && status.contains("smolder"),
        "expected both names in status, got: {status}"
    );
}

// ---------------------------------------------------------------------------
// Test 3: `:help <prefix>` completes a built-in command name
// ---------------------------------------------------------------------------

#[test]
fn help_completes_builtin() {
    let mut core = empty_core();

    core.handle_key(key(KeyCode::Char(':')));
    type_prompt(&mut core, "help op");

    core.handle_key(key(KeyCode::Tab));

    assert_eq!(core.command_buffer(), "help open");
}

// ---------------------------------------------------------------------------
// Test 4: `:help <prefix>` completes a plugin command name
// ---------------------------------------------------------------------------

#[test]
fn help_completes_plugin() {
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
    core.execute_command(&format!("plug-load {}", script.display()));

    core.handle_key(key(KeyCode::Char(':')));
    type_prompt(&mut core, "help r");

    core.handle_key(key(KeyCode::Tab));

    // The buffer should contain "help r" plus the longest common prefix
    // of all names starting with "r". At minimum "rc" should be among
    // the candidates shown in the status message.
    let status = core.status_message();
    assert!(
        status.contains("rc"),
        "expected 'rc' in completion candidates, got status: {status}"
    );
}

// ---------------------------------------------------------------------------
// Test 5: `:export <prefix>` completes the format
// ---------------------------------------------------------------------------

#[test]
fn export_completes_format() {
    let mut core = empty_core();

    core.handle_key(key(KeyCode::Char(':')));
    type_prompt(&mut core, "export c");

    core.handle_key(key(KeyCode::Tab));

    assert_eq!(core.command_buffer(), "export csv");
}

// ---------------------------------------------------------------------------
// Test 6: Unknown command head → Tab is a no-op
// ---------------------------------------------------------------------------

#[test]
fn unknown_head_is_noop() {
    let mut core = empty_core();

    core.handle_key(key(KeyCode::Char(':')));
    type_prompt(&mut core, "zz a");

    let before = core.command_buffer().to_owned();
    core.handle_key(key(KeyCode::Tab));

    assert_eq!(core.command_buffer(), before);
}

// ---------------------------------------------------------------------------
// Test 7: Bare `:` (empty buffer) → no completion
// ---------------------------------------------------------------------------

#[test]
fn bare_colon_no_completion() {
    let mut core = core_with_connections(&["smoke"]);

    core.handle_key(key(KeyCode::Char(':')));
    // Don't type anything — buffer is empty.

    let before = core.command_buffer().to_owned();
    core.handle_key(key(KeyCode::Tab));

    assert_eq!(core.command_buffer(), before);
}
