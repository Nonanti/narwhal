//! T1-T3-B — Workspace persistence integration tests.
//!
//! Covers:
//!
//! - Serde round-trip on the wire-format types (`schema_version`
//!   pinned to 1, optional fields collapse, kebab-case enums).
//! - `save_at_exit` → `load_at_start` ABI: write a snapshot, read it
//!   back, assert struct equality and that the atomic-rename leaves
//!   no temp files behind.
//! - Per-knob restore: `restore_tabs = false` keeps the default
//!   tab list; `enabled = false` short-circuits everything.
//! - `AppCore`-derived snapshot reflects live tab/cursor/scroll
//!   state.
//! - Forward-version snapshots surface as
//!   `PersistError::UnsupportedSchema` rather than silently
//!   replacing user state with defaults.
//! - Concurrent-instance lock fallback: while one writer holds the
//!   `.lock` sentinel, a second writer falls through to the per-pid
//!   path instead of clobbering the canonical file.

use std::path::PathBuf;
use std::sync::Arc;

use narwhal_app::core::AppCore;
use narwhal_app::persist::{
    self, CURRENT_SCHEMA_VERSION, PersistError, PersistedSidebar, PersistedTab, PersistedTabKind,
    PersistedWorkspace, SaveOutcome,
};
use narwhal_app::registry::DriverRegistry;
use narwhal_config::{ConnectionsFile, InMemoryStore, WorkspacePersistSettings};

fn sample_snapshot() -> PersistedWorkspace {
    PersistedWorkspace::with(|w| {
        w.narwhal_version = Some("1.2.0".into());
        w.saved_at = Some("0s-since-epoch".into());
        w.active_connection = Some("prod-pg".into());
        w.tabs = vec![
            PersistedTab::with(|t| {
                t.name = "untitled-1".into();
                t.buffer = "SELECT 1;\n-- second line".into();
                t.cursor_row = 1;
                t.cursor_col = 5;
            }),
            PersistedTab::with(|t| {
                t.name = "untitled-2".into();
            }),
        ];
        w.active_tab = 1;
        w.sidebar = PersistedSidebar::with(|s| {
            s.selected_index = 3;
            s.scroll = 2;
        });
    })
}

#[test]
fn serde_roundtrip_preserves_every_field() {
    let snapshot = sample_snapshot();
    let text = toml::to_string_pretty(&snapshot).unwrap();
    let parsed: PersistedWorkspace = toml::from_str(&text).unwrap();
    assert_eq!(parsed, snapshot);
}

#[test]
fn serialised_form_pins_kebab_case_kind_and_collapses_empty_optionals() {
    let snapshot = PersistedWorkspace::with(|w| {
        w.tabs.push(PersistedTab::with(|t| {
            t.name = "scratch".into();
        }));
    });
    let text = toml::to_string_pretty(&snapshot).unwrap();
    // Kebab-case enum spelling — guards against the
    // `#[serde(rename_all = "kebab-case")]` attribute being dropped
    // by a future refactor.
    assert!(text.contains("kind = \"sql-editor\""), "{text}");
    // Empty optionals stay absent — keeps the file readable for
    // first-run users.
    assert!(!text.contains("narwhal_version"), "{text}");
    assert!(!text.contains("active_connection"), "{text}");
    assert!(!text.contains("expanded_schemas"), "{text}");
}

#[test]
fn save_then_load_returns_identical_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("workspace-state.toml");
    let snapshot = sample_snapshot();

    let outcome = persist::save_at_exit(&snapshot, &path).expect("save");
    assert_eq!(outcome, SaveOutcome::Canonical(path.clone()));

    let loaded = persist::load_at_start(&path).expect("load").expect("some");
    assert_eq!(loaded, snapshot);

    // The atomic write must leave no `.narwhal-*.tmp` straggler
    // behind. A single rename is the contract.
    for entry in std::fs::read_dir(dir.path()).unwrap().flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        assert!(
            !name.starts_with(".narwhal-"),
            "temp file left behind: {name}"
        );
    }
}

#[test]
fn load_missing_file_returns_none() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("nope.toml");
    let loaded = persist::load_at_start(&path).expect("missing-file = ok");
    assert!(loaded.is_none());
}

#[test]
fn load_forward_schema_version_returns_unsupported() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("workspace-state.toml");
    let snapshot = PersistedWorkspace::with(|w| {
        w.schema_version = CURRENT_SCHEMA_VERSION + 99;
        w.active_connection = Some("ignored".into());
    });
    let text = toml::to_string_pretty(&snapshot).unwrap();
    std::fs::write(&path, text).unwrap();

    let err = persist::load_at_start(&path).unwrap_err();
    match err {
        PersistError::UnsupportedSchema { found, supported } => {
            assert_eq!(found, CURRENT_SCHEMA_VERSION + 99);
            assert_eq!(supported, CURRENT_SCHEMA_VERSION);
        }
        other => panic!("expected UnsupportedSchema, got {other:?}"),
    }
}

fn empty_core() -> AppCore {
    let registry = DriverRegistry::new();
    AppCore::with_credentials(
        registry,
        ConnectionsFile::default(),
        None,
        Arc::new(InMemoryStore::new()),
    )
}

#[test]
fn snapshot_projects_live_editor_state() {
    let mut core = empty_core();
    // Mutate the default tab through the public editor API.
    {
        let editor = core.tabs_mut()[0].editor_mut();
        editor.insert_str("SELECT *\nFROM users\nWHERE id = 1;");
        editor.set_cursor(1, 4);
        editor.set_scroll(0);
    }
    let snapshot = persist::snapshot(&core);
    assert_eq!(snapshot.schema_version, CURRENT_SCHEMA_VERSION);
    assert_eq!(snapshot.tabs.len(), 1);
    let tab = &snapshot.tabs[0];
    assert_eq!(tab.buffer, "SELECT *\nFROM users\nWHERE id = 1;");
    assert_eq!(tab.cursor_row, 1);
    assert_eq!(tab.cursor_col, 4);
    assert_eq!(tab.kind, PersistedTabKind::SqlEditor);
    assert!(snapshot.active_connection.is_none());
}

#[test]
fn apply_restores_tabs_cursor_and_scroll() {
    let mut core = empty_core();
    let snapshot = PersistedWorkspace::with(|w| {
        w.active_connection = Some("prod-pg".into());
        w.tabs = vec![
            PersistedTab::with(|t| {
                t.name = "alpha".into();
                t.buffer = "SELECT 1;".into();
                t.cursor_col = 7;
            }),
            PersistedTab::with(|t| {
                t.name = "beta".into();
                t.buffer = "line-1\nline-2\nline-3".into();
                t.cursor_row = 2;
                t.cursor_col = 4;
                t.scroll = 1;
            }),
        ];
        w.active_tab = 1;
        w.sidebar = PersistedSidebar::with(|s| {
            s.selected_index = 5;
            s.scroll = 2;
        });
    });
    let pending = persist::apply(&mut core, snapshot, &WorkspacePersistSettings::default());
    assert_eq!(pending.as_deref(), Some("prod-pg"));
    assert_eq!(core.tabs().len(), 2);
    assert_eq!(core.active_tab(), 1);

    let alpha = &core.tabs()[0];
    assert_eq!(alpha.name(), "alpha");
    assert_eq!(alpha.editor().entire_text(), "SELECT 1;");
    assert_eq!(alpha.editor().cursor(), (0, 7));

    let beta = &core.tabs()[1];
    assert_eq!(beta.name(), "beta");
    assert_eq!(beta.editor().entire_text(), "line-1\nline-2\nline-3");
    assert_eq!(beta.editor().cursor(), (2, 4));
    assert_eq!(beta.editor().scroll(), 1);

    assert_eq!(core.ui_for_test().sidebar_index, 5);
    assert_eq!(core.ui_for_test().sidebar_scroll, 2);
}

#[test]
fn apply_respects_disabled_persist_setting() {
    let mut core = empty_core();
    let snapshot = sample_snapshot();
    let mut settings = WorkspacePersistSettings::default();
    settings.enabled = false;
    let pending = persist::apply(&mut core, snapshot, &settings);
    assert!(pending.is_none());
    // The default lonely-untitled-1 tab is still in place.
    assert_eq!(core.tabs().len(), 1);
    assert_eq!(core.tabs()[0].name(), "untitled-1");
}

#[test]
fn apply_with_restore_tabs_off_keeps_default_tabs() {
    let mut core = empty_core();
    let snapshot = sample_snapshot();
    let mut settings = WorkspacePersistSettings::default();
    settings.restore_tabs = false;
    let pending = persist::apply(&mut core, snapshot, &settings);
    // The connection name still surfaces — the brief makes that
    // unconditional on `enabled = true`.
    assert_eq!(pending.as_deref(), Some("prod-pg"));
    // But the runtime tab list is untouched: still the single
    // untitled-1 tab from `UiState::new`.
    assert_eq!(core.tabs().len(), 1);
    assert_eq!(core.tabs()[0].name(), "untitled-1");
}

#[test]
fn apply_with_restore_cursor_off_keeps_buffer_but_resets_caret() {
    let mut core = empty_core();
    let snapshot = PersistedWorkspace::with(|w| {
        w.tabs = vec![PersistedTab::with(|t| {
            t.name = "alpha".into();
            t.buffer = "line-1\nline-2\nline-3".into();
            t.cursor_row = 2;
            t.cursor_col = 4;
            t.scroll = 1;
        })];
    });
    let mut settings = WorkspacePersistSettings::default();
    settings.restore_cursor = false;
    let _ = persist::apply(&mut core, snapshot, &settings);
    assert_eq!(core.tabs().len(), 1);
    let tab = &core.tabs()[0];
    // Buffer text restored.
    assert_eq!(tab.editor().entire_text(), "line-1\nline-2\nline-3");
    // Cursor and scroll reset to the top because the user opted
    // out of cursor restore.
    assert_eq!(tab.editor().cursor(), (0, 0));
    assert_eq!(tab.editor().scroll(), 0);
}

#[test]
fn apply_clamps_oob_cursor_and_active_tab() {
    let mut core = empty_core();
    let snapshot = PersistedWorkspace::with(|w| {
        w.tabs = vec![PersistedTab::with(|t| {
            t.name = "only".into();
            t.buffer = "short".into();
            t.cursor_row = 999;
            t.cursor_col = 999;
        })];
        w.active_tab = 999;
    });
    let _ = persist::apply(&mut core, snapshot, &WorkspacePersistSettings::default());
    assert_eq!(core.tabs().len(), 1);
    assert_eq!(core.active_tab(), 0);
    let (row, col) = core.tabs()[0].editor().cursor();
    // Clamped into bounds without panicking.
    assert_eq!(row, 0);
    assert_eq!(col, "short".len());
}

#[test]
fn concurrent_writers_fall_back_to_per_pid_path() {
    let dir = tempfile::tempdir().unwrap();
    let canonical = dir.path().join("workspace-state.toml");

    // Simulate an already-running writer by manually creating the
    // lock sentinel. The second `save_at_exit` call must see this
    // as contention and steer the snapshot to the per-pid path.
    let lock = persist::paths::lock_path(&canonical);
    std::fs::write(&lock, "1\n").unwrap();

    let snapshot = sample_snapshot();
    let outcome = persist::save_at_exit(&snapshot, &canonical).expect("save");
    let pid_path: PathBuf = persist::paths::per_pid_path(&canonical, std::process::id());
    assert_eq!(outcome, SaveOutcome::PerPid(pid_path.clone()));
    assert!(pid_path.exists(), "per-pid file should be present");
    // Canonical was *not* clobbered (it didn't exist before; still
    // shouldn't).
    assert!(!canonical.exists(), "canonical file must not appear");

    std::fs::remove_file(&lock).unwrap();
}
