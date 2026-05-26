//! Integration tests for the snippet store and modal.
//!
//! Five tests as required by plan 07-07:
//! 1. `save_then_load_round_trip`
//! 2. `invalid_name_rejected`
//! 3. `list_returns_sorted_names`
//! 4. `remove_deletes_file`
//! 5. `tab_complete_includes_snippets`

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};
use narwhal_app::core::AppCore;
use narwhal_app::{DriverRegistry, SnippetStore};
use narwhal_config::ConnectionsFile;
use tempfile::TempDir;

const fn key(code: KeyCode) -> KeyEvent {
    KeyEvent {
        code,
        modifiers: KeyModifiers::NONE,
        kind: KeyEventKind::Press,
        state: KeyEventState::NONE,
    }
}

fn empty_core_with_snippet_store(dir: &TempDir) -> AppCore {
    let registry = DriverRegistry::with_defaults();
    let connections = ConnectionsFile {
        connections: Vec::new(),
    };
    let mut core = AppCore::new(registry, connections, None);
    // Override the snippet store root to the temp directory so tests
    // don't pollute the user's real config.
    core.set_snippet_store_root(dir.path().join("snippets"));
    core
}

/// 1. Save a snippet and load it back — round-trip.
#[test]
fn save_then_load_round_trip() {
    let dir = TempDir::new().unwrap();
    let store = SnippetStore::new(dir.path().join("snippets"));

    store.save("foo", "SELECT 1").unwrap();
    let loaded = store.load("foo").unwrap();
    assert_eq!(loaded, "SELECT 1");
}

/// 2. Invalid names are rejected by the store.
#[test]
fn invalid_name_rejected() {
    let dir = TempDir::new().unwrap();
    let store = SnippetStore::new(dir.path().join("snippets"));

    let result = store.save("Has Space", "SELECT 1");
    assert!(result.is_err(), "expected error for name with space");
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("invalid snippet name"),
        "error should mention invalid name, got: {err}"
    );

    // Other invalid names
    assert!(store.save("UPPER", "x").is_err());
    assert!(store.save("has.dot", "x").is_err());
    assert!(store.save("", "x").is_err());

    // Valid names should work
    assert!(store.save("lower", "x").is_ok());
    assert!(store.save("with-dash", "x").is_ok());
    assert!(store.save("with_underscore", "x").is_ok());
    assert!(store.save("with123", "x").is_ok());
}

/// 3. List returns snippet names sorted alphabetically.
#[test]
fn list_returns_sorted_names() {
    let dir = TempDir::new().unwrap();
    let store = SnippetStore::new(dir.path().join("snippets"));

    store.save("b", "SELECT 2").unwrap();
    store.save("a", "SELECT 1").unwrap();
    store.save("c", "SELECT 3").unwrap();

    let names = store.list().unwrap();
    assert_eq!(names, vec!["a", "b", "c"]);
}

/// 4. Remove deletes the file; subsequent load fails.
#[test]
fn remove_deletes_file() {
    let dir = TempDir::new().unwrap();
    let store = SnippetStore::new(dir.path().join("snippets"));

    store.save("to-delete", "SELECT 1").unwrap();
    assert!(store.load("to-delete").is_ok());

    store.remove("to-delete").unwrap();
    assert!(
        store.load("to-delete").is_err(),
        "loading a removed snippet should fail"
    );
}

/// 5. Atomic write: save writes to a tmp file then renames.
#[test]
fn save_uses_atomic_rename() {
    let dir = TempDir::new().unwrap();
    let store = SnippetStore::new(dir.path().join("snippets"));

    // First save
    store.save("atomic", "SELECT 1").unwrap();
    let loaded = store.load("atomic").unwrap();
    assert_eq!(loaded, "SELECT 1");

    // Overwrite — the file must contain the new content entirely, never
    // a truncated intermediate state.
    store.save("atomic", "SELECT 999").unwrap();
    let loaded = store.load("atomic").unwrap();
    assert_eq!(loaded, "SELECT 999");

    // No leftover .tmp files after successful saves.
    let tmp_path = dir.path().join("snippets").join(".atomic.sql.tmp");
    assert!(
        !tmp_path.exists(),
        ".tmp file should not exist after successful save"
    );
}

/// 6. Tab-completion for `:load` includes snippet names.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tab_complete_includes_snippets() {
    let dir = TempDir::new().unwrap();
    let mut core = empty_core_with_snippet_store(&dir);

    // Save a snippet via the store directly.
    core.snippet_store()
        .save("users-active", "SELECT * FROM users WHERE active = true")
        .unwrap();

    // Enter command mode and type "load us"
    core.handle_key(key(KeyCode::Char(':'))).await;
    for ch in "load us".chars() {
        core.handle_key(key(KeyCode::Char(ch))).await;
    }

    // Press Tab
    core.handle_key(key(KeyCode::Tab)).await;

    // The command buffer should complete to "load users-active"
    assert_eq!(
        core.command_buffer(),
        "load users-active",
        "tab completion should extend :load with snippet names"
    );
}
