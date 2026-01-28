//! Path completion for sqlite/duckdb path fields and the SSL cert
//! family. The wizard exposes [`PathCompletion`] so we can assert on
//! the variant returned by [`ConnectionWizard::complete_focused_path`]
//! without having to render anything.

use std::fs;

use narwhal_app::wizard::{ConnectionWizard, PathCompletion, WizardFieldKind, WizardFieldValue};
use tempfile::TempDir;

fn focus_path(w: &mut ConnectionWizard) {
    let idx = w
        .fields
        .iter()
        .position(|f| f.kind == WizardFieldKind::Path)
        .expect("path field on sqlite wizard");
    w.focused = idx + 1;
}

fn set_path(w: &mut ConnectionWizard, value: &str) {
    let idx = w
        .fields
        .iter()
        .position(|f| f.kind == WizardFieldKind::Path)
        .unwrap();
    w.fields[idx].value = WizardFieldValue::Public(value.into());
}

fn get_path(w: &ConnectionWizard) -> String {
    let idx = w
        .fields
        .iter()
        .position(|f| f.kind == WizardFieldKind::Path)
        .unwrap();
    w.fields[idx].value.expose().to_owned()
}

/// Single match → field is rewritten to the absolute path.
#[test]
fn single_match_completes_to_absolute_path() {
    let dir = TempDir::new().unwrap();
    let db = dir.path().join("only.db");
    fs::write(&db, b"").unwrap();

    let mut w = ConnectionWizard::new(); // defaults to sqlite (Name, Path)
    focus_path(&mut w);
    set_path(&mut w, &format!("{}/onl", dir.path().display()));
    let outcome = w.complete_focused_path();
    assert_eq!(outcome, PathCompletion::Single);
    assert_eq!(get_path(&w), db.to_string_lossy());
}

/// Multiple candidates with a shared prefix → the field is extended
/// to the longest common prefix and the count is reported.
#[test]
fn multiple_matches_extend_to_common_prefix() {
    let dir = TempDir::new().unwrap();
    for name in ["alpha.db", "alpine.db", "beta.db"] {
        fs::write(dir.path().join(name), b"").unwrap();
    }
    let mut w = ConnectionWizard::new();
    focus_path(&mut w);
    set_path(&mut w, &format!("{}/al", dir.path().display()));
    let outcome = w.complete_focused_path();
    match outcome {
        PathCompletion::Multiple { count, samples } => {
            assert_eq!(count, 2);
            assert!(samples.contains(&"alpha.db".to_owned()));
            assert!(samples.contains(&"alpine.db".to_owned()));
        }
        other => panic!("expected Multiple, got {other:?}"),
    }
    // Common prefix of "alpha.db" and "alpine.db" is "alp".
    assert!(get_path(&w).ends_with("/alp"), "got: {}", get_path(&w));
}

/// No matches → field unchanged, `NoMatch` reported.
#[test]
fn no_match_leaves_field_intact() {
    let dir = TempDir::new().unwrap();
    let mut w = ConnectionWizard::new();
    focus_path(&mut w);
    let input = format!("{}/zzz_does_not_exist", dir.path().display());
    set_path(&mut w, &input);
    let outcome = w.complete_focused_path();
    assert_eq!(outcome, PathCompletion::NoMatch);
    assert_eq!(get_path(&w), input);
}

/// Completion is only wired up for path-shaped fields. Asking on the
/// Name field is a no-op (`NoMatch` + value untouched).
#[test]
fn non_path_field_is_a_noop() {
    let mut w = ConnectionWizard::new();
    // Focus the Name field (index 0 → focused = 1).
    w.focused = 1;
    w.fields[0].value = WizardFieldValue::Public("hello".into());
    let outcome = w.complete_focused_path();
    assert_eq!(outcome, PathCompletion::NoMatch);
    assert_eq!(w.fields[0].value.expose(), "hello");
}

/// Trailing slash means "list this directory" — completion returns
/// every child as Multiple (or Single if the dir has exactly one).
#[test]
fn trailing_slash_lists_directory_children() {
    let dir = TempDir::new().unwrap();
    fs::write(dir.path().join("a.db"), b"").unwrap();
    fs::write(dir.path().join("b.db"), b"").unwrap();
    let mut w = ConnectionWizard::new();
    focus_path(&mut w);
    set_path(&mut w, &format!("{}/", dir.path().display()));
    let outcome = w.complete_focused_path();
    match outcome {
        PathCompletion::Multiple { count, .. } => assert_eq!(count, 2),
        other => panic!("expected Multiple, got {other:?}"),
    }
}
