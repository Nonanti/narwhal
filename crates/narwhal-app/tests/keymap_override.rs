//! L36 #4 v1 integration: user-supplied `[keymap.<group>]` overrides
//! end up in the live dispatch keymap.
//!
//! Verifies the full chain `Settings::load_from_str` →
//! `AppCore::apply_settings` → `Keymap::apply_overrides` →
//! `Keymap::resolve`. Two cases are exercised:
//!
//! 1. Rebinding `d` from `results-delete-row` to
//!    `results-discard-pending` (i.e. taking the chord away from CRUD
//!    and giving it to the queue) actually changes what fires.
//! 2. Malformed entries collect into the `keymap_warnings` channel
//!    instead of panicking, so a typo in one line doesn't break the
//!    rest of the file.

use crossterm::event::{KeyCode, KeyModifiers};
use narwhal_app::DriverRegistry;
use narwhal_app::core::AppCore;
use narwhal_commands::action::{Action, KeyGroup};
use narwhal_commands::keymap::KeyChord;
use narwhal_config::{ConnectionsFile, Settings};

fn settings_from_toml(toml_text: &str) -> Settings {
    Settings::load_from_str(toml_text).expect("settings parse")
}

#[test]
fn rebinds_d_from_delete_to_discard_pending() {
    let registry = DriverRegistry::with_defaults();
    let connections = ConnectionsFile::default();
    let mut core = AppCore::new(registry, connections, None);
    let toml = r#"
[keymap.results]
"d" = "results-discard-pending"
"#;
    let settings = settings_from_toml(toml);
    core.apply_settings(settings);

    let resolved = core.keymap().resolve(KeyGroup::Results, KeyChord::ch('d'));
    assert_eq!(
        resolved,
        Some(Action::ResultsDiscardPending),
        "d should now fire ResultsDiscardPending, not ResultsDeleteRow"
    );
    // Sanity: the default Ctrl-X still discards (we only rebound `d`).
    let still_default = core
        .keymap()
        .resolve(KeyGroup::Results, KeyChord::ctrl('x'));
    assert_eq!(still_default, Some(Action::ResultsDiscardPending));
}

#[test]
fn malformed_overrides_become_warnings_not_panics() {
    let registry = DriverRegistry::with_defaults();
    let connections = ConnectionsFile::default();
    let mut core = AppCore::new(registry, connections, None);
    let toml = r#"
[keymap.results]
"d" = "this-action-does-not-exist"
"not-a-chord-!@#" = "results-delete-row"
"#;
    let settings = settings_from_toml(toml);
    core.apply_settings(settings);

    let warnings = core.keymap_warnings();
    assert!(
        warnings.len() >= 2,
        "both broken lines must produce a warning; got: {warnings:?}"
    );
    // d still maps to the *default* action because the override was
    // rejected.
    assert_eq!(
        core.keymap().resolve(KeyGroup::Results, KeyChord::ch('d')),
        Some(Action::ResultsDeleteRow)
    );
}

#[test]
fn empty_keymap_section_leaves_defaults_intact() {
    let registry = DriverRegistry::with_defaults();
    let connections = ConnectionsFile::default();
    let mut core = AppCore::new(registry, connections, None);
    let toml = "# nothing here\n";
    let settings = settings_from_toml(toml);
    core.apply_settings(settings);

    // Spot-check every L36 chord.
    let map = core.keymap();
    assert_eq!(
        map.resolve(KeyGroup::Results, KeyChord::ch('o')),
        Some(Action::ResultsAppendRow)
    );
    assert_eq!(
        map.resolve(KeyGroup::Results, KeyChord::shift('o')),
        Some(Action::ResultsDuplicateRow)
    );
    assert_eq!(
        map.resolve(KeyGroup::Results, KeyChord::ch('z')),
        Some(Action::OpenJsonViewerCell)
    );
    assert_eq!(
        map.resolve(KeyGroup::Results, KeyChord::ctrl('s')),
        Some(Action::ResultsCommitPending)
    );

    // The Enter -> cell popup binding (no modifier) still resolves so
    // that the cell editor path is not accidentally regressed.
    let enter = KeyChord::new(KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(
        map.resolve(KeyGroup::Results, enter),
        Some(Action::ResultsOpenCellPopup)
    );
}
