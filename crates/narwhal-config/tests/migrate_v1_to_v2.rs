//! End-to-end tests for the v1 → v2 settings + connections
//! migration produced by `narwhal-config::migrate`.
//!
//! Covers:
//! - lossless transform of every v1 settings field
//! - idempotence (running migration twice is a no-op)
//! - backup creation, refusal to overwrite, `--force` override
//! - dry-run does not touch disk
//! - the v2 file round-trips through `Settings::load`

use std::fs;

use narwhal_config::{
    CURRENT_SCHEMA_VERSION, ConnectionsFile, MigrateOptions, MigrateOutcome, Settings,
    ValidateOutcome, migrate_config, migrate_connections, migrate_settings, render_settings_v2,
    validate_config,
};

// Kitchen-sink v1 settings fixture. Every v1 field is set to a
// **non-default** value so a silent drop during migration would
// flip the round-trip assertion below. Future contributors who add
// a new v2 field do NOT touch this fixture — they add a separate
// `additive_*` test that asserts the new field defaults correctly
// for v1 input.
const KITCHEN_SINK_V1_SETTINGS: &str = r#"
theme = "light"

[editor]
tab_width = 2
use_spaces = false
line_numbers = false

[keybindings]
vim_mode = false

[diagram]
icons = "nerdfont"

[keymap.results]
"ctrl+s"   = "results-commit-pending"
"K"        = "results-prev-tab"
"shift+tab" = "results-prev-cell"

[keymap.row-detail]
"esc" = "row-detail-close"
"j"   = "row-detail-next-field"
"k"   = "row-detail-prev-field"

[keymap.editor]
"ctrl+space" = "editor-trigger-completion"
"#;

// Kitchen-sink v1 connections fixture. Three connections cover the
// shapes that mattered in v1.x: a fully-spec'd postgres entry with
// TLS, SSH, color, write-confirmation, pre-connect, and an inline
// password env var; a minimal sqlite entry that exercises the
// path-only branch; and a mysql entry with options + non-default
// SSL mode. Two logical relations test both single-column and
// composite-column shapes.
const KITCHEN_SINK_V1_CONNECTIONS: &str = r#"
[[connection]]
id     = "00000000-0000-0000-0000-000000000001"
name   = "prod-pg"
driver = "postgres"

[connection.params]
host           = "db.prod.example.com"
port           = 5432
database       = "appdb"
username       = "app_ro"
ssl_mode       = "verify-full"
ssl_root_cert  = "/etc/ssl/certs/prod-ca.pem"
color          = "red"
confirm_writes = true
read_only      = true

[connection.params.options]
statement_timeout = "30000"
application_name  = "narwhal"

[connection.params.ssh]
host = "bastion.prod.example.com"
user = "deploy"
port = 2222

[[connection.params.pre_connect]]
command        = "vault kv get -field=password secret/prod-pg"
save_output_to = "vault_pw"
timeout_secs   = 10
required       = true

[[connection]]
id     = "00000000-0000-0000-0000-000000000002"
name   = "local-sqlite"
driver = "sqlite"

[connection.params]
path = "/home/alice/scratch.db"

[[connection]]
id     = "00000000-0000-0000-0000-000000000003"
name   = "reporting-mysql"
driver = "mysql"

[connection.params]
host     = "reports.example.com"
port     = 3306
database = "analytics"
username = "reader"
ssl_mode = "require"

[connection.params.options]
net_read_timeout = "60"

[[logical_relation]]
connection  = "prod-pg"
from        = "events.user_id"
to          = "users.id"
cardinality = "many-to-one"
note        = "cross-shard, no real FK"

[[logical_relation]]
connection   = "prod-pg"
from_columns = ["order_items.order_id", "order_items.line_no"]
to_columns   = ["orders.id", "orders.line_no"]
cardinality  = "many-to-one"
"#;

fn temp_dir() -> tempfile::TempDir {
    tempfile::tempdir().expect("create temp dir")
}

#[test]
fn settings_load_v1_returns_needs_migration() {
    let dir = temp_dir();
    let path = dir.path().join("settings.toml");
    fs::write(&path, KITCHEN_SINK_V1_SETTINGS).unwrap();

    match Settings::load(&path) {
        Err(narwhal_config::ConfigError::NeedsMigration { path: p }) => {
            assert_eq!(p, path);
        }
        other => panic!("expected NeedsMigration, got {other:?}"),
    }
}

#[test]
fn connections_load_v1_returns_needs_migration() {
    let dir = temp_dir();
    let path = dir.path().join("connections.toml");
    fs::write(&path, KITCHEN_SINK_V1_CONNECTIONS).unwrap();

    match ConnectionsFile::load(&path) {
        Err(narwhal_config::ConfigError::NeedsMigration { path: p }) => {
            assert_eq!(p, path);
        }
        other => panic!("expected NeedsMigration, got {other:?}"),
    }
}

#[test]
fn settings_migrate_preserves_every_v1_field() {
    let dir = temp_dir();
    let path = dir.path().join("settings.toml");
    fs::write(&path, KITCHEN_SINK_V1_SETTINGS).unwrap();

    let outcome = migrate_settings(&path, &MigrateOptions::default()).unwrap();
    match outcome {
        MigrateOutcome::Migrated {
            backup_path,
            rendered_v2: _,
        } => {
            assert!(backup_path.as_ref().is_some_and(|p| p.exists()));
        }
        other => panic!("expected Migrated, got {other:?}"),
    }

    // Reload via the canonical v2 loader and assert every NON-default
    // v1 field made it across. The fixture deliberately uses
    // non-default values for every v1 knob, so a silent drop would
    // surface as a flipped assertion below.
    let loaded = Settings::load(&path).expect("v2 reload");
    assert_eq!(loaded.theme, narwhal_config::Theme::Light);
    assert_eq!(loaded.editor.tab_width, 2);
    assert!(!loaded.editor.use_spaces);
    assert!(!loaded.editor.line_numbers);
    // v1 fixture still carries the deprecated `vim_mode = false`
    // bit. We assert it round-trips for back-compat — the soft
    // migration to `editor.mode = "basic"` happens at runtime in
    // `apply_settings`, not in the on-disk migration.
    #[allow(deprecated)]
    {
        assert!(!loaded.keybindings.vim_mode);
    }
    // v2 additions land at their defaults.
    assert_eq!(loaded.editor.mode, narwhal_config::EditorMode::Vim);
    assert_eq!(
        loaded.editor.mouse,
        narwhal_config::MouseSelectionMode::Enabled
    );
    assert_eq!(
        loaded.keybindings.preset,
        narwhal_config::KeyPreset::Default
    );
    assert_eq!(loaded.diagram.icons, narwhal_config::DiagramIcons::Nerdfont);
    // keymap: three groups, eight bindings total.
    assert_eq!(loaded.keymap.len(), 3, "keymap groups");
    assert_eq!(
        loaded
            .keymap
            .get("results")
            .map(std::collections::HashMap::len),
        Some(3),
    );
    assert_eq!(
        loaded
            .keymap
            .get("results")
            .and_then(|m| m.get("ctrl+s"))
            .map(String::as_str),
        Some("results-commit-pending"),
    );
    assert_eq!(
        loaded
            .keymap
            .get("row-detail")
            .and_then(|m| m.get("esc"))
            .map(String::as_str),
        Some("row-detail-close"),
    );
    assert_eq!(
        loaded
            .keymap
            .get("editor")
            .and_then(|m| m.get("ctrl+space"))
            .map(String::as_str),
        Some("editor-trigger-completion"),
    );
    // v2 additions land at their defaults — nothing got silently
    // dropped, but no new content appeared either.
    // T1-T3-B: workspace-state persistence is opt-out, so the
    // migrated settings inherit the new `true` defaults. Older
    // assertions used to check `false` here; updated alongside
    // the v2.0 default flip.
    assert!(loaded.workspace.persist.enabled);
    assert!(loaded.workspace.persist.restore_tabs);
    assert!(loaded.workspace.persist.restore_cursor);
    assert!(loaded.workspace.persist.restore_sidebar);
    assert_eq!(
        loaded.vault.default_provider,
        narwhal_config::VaultProvider::None
    );
    assert!(!loaded.plugins.wasm.enabled);
    assert!(loaded.plugins.lua_dir.is_none());
}

#[test]
fn connections_migrate_injects_schema_version() {
    let dir = temp_dir();
    let path = dir.path().join("connections.toml");
    fs::write(&path, KITCHEN_SINK_V1_CONNECTIONS).unwrap();

    migrate_connections(&path, &MigrateOptions::default()).unwrap();

    let raw = fs::read_to_string(&path).unwrap();
    assert!(
        raw.contains(&format!("schema_version = {CURRENT_SCHEMA_VERSION}")),
        "v2 connections.toml is missing the schema_version header:\n{raw}",
    );

    // ConnectionsFile::load now succeeds and the full kitchen-sink
    // payload survives the round trip.
    let loaded = ConnectionsFile::load(&path).expect("v2 reload");
    assert_eq!(loaded.connections.len(), 3);
    assert_eq!(loaded.logical_relations.len(), 2);

    // Connection 1: prod-pg — every non-default knob preserved.
    let pg = &loaded.connections[0];
    assert_eq!(pg.name, "prod-pg");
    assert_eq!(pg.driver, "postgres");
    assert_eq!(pg.params.host.as_deref(), Some("db.prod.example.com"));
    assert_eq!(pg.params.port, Some(5432));
    assert_eq!(pg.params.database.as_deref(), Some("appdb"));
    assert_eq!(pg.params.username.as_deref(), Some("app_ro"));
    assert_eq!(pg.params.ssl_mode, narwhal_core::SslMode::VerifyFull);
    assert!(
        pg.params
            .ssl_root_cert
            .as_ref()
            .is_some_and(|p| p.to_string_lossy().contains("prod-ca.pem"))
    );
    assert_eq!(pg.params.color, Some(narwhal_core::ConnectionColor::Red));
    assert!(pg.params.confirm_writes);
    assert!(pg.params.read_only);
    assert_eq!(
        pg.params
            .options
            .get("statement_timeout")
            .map(String::as_str),
        Some("30000"),
    );
    assert_eq!(
        pg.params
            .options
            .get("application_name")
            .map(String::as_str),
        Some("narwhal"),
    );
    let ssh = pg.params.ssh.as_ref().expect("ssh block preserved");
    assert_eq!(ssh.host, "bastion.prod.example.com");
    assert_eq!(ssh.user, "deploy");
    assert_eq!(ssh.port, Some(2222));
    assert_eq!(pg.params.pre_connect.len(), 1);
    assert_eq!(
        pg.params.pre_connect[0].command,
        "vault kv get -field=password secret/prod-pg"
    );
    assert_eq!(
        pg.params.pre_connect[0].save_output_to.as_deref(),
        Some("vault_pw"),
    );
    assert_eq!(pg.params.pre_connect[0].timeout_secs, Some(10));
    assert!(pg.params.pre_connect[0].required);

    // Connection 2: local-sqlite — minimal path-only.
    let sqlite = &loaded.connections[1];
    assert_eq!(sqlite.name, "local-sqlite");
    assert_eq!(sqlite.driver, "sqlite");
    assert_eq!(
        sqlite.params.path.as_deref(),
        Some("/home/alice/scratch.db")
    );

    // Connection 3: reporting-mysql — options + non-default SSL.
    let mysql = &loaded.connections[2];
    assert_eq!(mysql.name, "reporting-mysql");
    assert_eq!(mysql.driver, "mysql");
    assert_eq!(mysql.params.ssl_mode, narwhal_core::SslMode::Require);
    assert_eq!(
        mysql
            .params
            .options
            .get("net_read_timeout")
            .map(String::as_str),
        Some("60"),
    );

    // Logical relations: simple + composite both round-trip.
    let rel_simple = &loaded.logical_relations[0];
    assert_eq!(rel_simple.connection, "prod-pg");
    assert_eq!(rel_simple.from.as_deref(), Some("events.user_id"));
    assert_eq!(rel_simple.to.as_deref(), Some("users.id"));
    assert_eq!(rel_simple.cardinality, "many-to-one");
    assert_eq!(rel_simple.note.as_deref(), Some("cross-shard, no real FK"));

    let rel_composite = &loaded.logical_relations[1];
    assert_eq!(rel_composite.from_columns.len(), 2);
    assert_eq!(rel_composite.to_columns.len(), 2);
    assert_eq!(rel_composite.from_columns[0], "order_items.order_id");
    assert_eq!(rel_composite.to_columns[1], "orders.line_no");
}

#[test]
fn migrate_is_idempotent() {
    let dir = temp_dir();
    let settings_path = dir.path().join("settings.toml");
    let connections_path = dir.path().join("connections.toml");
    fs::write(&settings_path, KITCHEN_SINK_V1_SETTINGS).unwrap();
    fs::write(&connections_path, KITCHEN_SINK_V1_CONNECTIONS).unwrap();

    let opts = MigrateOptions::with(|o| {
        // First run uses a backup; second run should hit the
        // already-v2 branch before checking the backup at all.
        o.force = false;
    });
    migrate_config(&settings_path, &connections_path, &opts).unwrap();

    let report2 = migrate_config(&settings_path, &connections_path, &opts).unwrap();
    assert!(matches!(report2.settings, MigrateOutcome::AlreadyV2));
    assert!(matches!(report2.connections, MigrateOutcome::AlreadyV2));
}

#[test]
fn migrate_refuses_to_overwrite_backup_without_force() {
    let dir = temp_dir();
    let path = dir.path().join("settings.toml");
    fs::write(&path, KITCHEN_SINK_V1_SETTINGS).unwrap();
    fs::write(format!("{}.v1.bak", path.display()), "stale-backup").unwrap();

    let err = migrate_settings(&path, &MigrateOptions::default()).unwrap_err();
    assert!(
        format!("{err}").contains("backup already exists"),
        "wrong error: {err}",
    );

    // The original v1 file is still on disk.
    let text = fs::read_to_string(&path).unwrap();
    assert!(!text.contains("schema_version"));

    // With --force the same call succeeds and overwrites the backup.
    let opts = MigrateOptions::with(|o| o.force = true);
    migrate_settings(&path, &opts).unwrap();
    let backup = fs::read_to_string(format!("{}.v1.bak", path.display())).unwrap();
    assert!(
        backup.contains("[editor]"),
        "backup was not refreshed: {backup}",
    );
}

#[test]
fn migrate_dry_run_does_not_touch_disk() {
    let dir = temp_dir();
    let path = dir.path().join("settings.toml");
    fs::write(&path, KITCHEN_SINK_V1_SETTINGS).unwrap();
    let before = fs::read_to_string(&path).unwrap();

    let opts = MigrateOptions::with(|o| o.dry_run = true);
    let outcome = migrate_settings(&path, &opts).unwrap();
    let MigrateOutcome::Migrated {
        backup_path,
        rendered_v2,
    } = outcome
    else {
        panic!("expected Migrated outcome in dry-run");
    };
    assert!(backup_path.is_none(), "dry-run must not create a backup");
    assert!(rendered_v2.contains("schema_version = 2"));

    let after = fs::read_to_string(&path).unwrap();
    assert_eq!(before, after, "dry-run touched the file");
}

#[test]
fn render_settings_v2_starts_with_schema_header() {
    let mut settings = Settings::default();
    settings.editor.tab_width = 2;
    let rendered = render_settings_v2(&settings).unwrap();
    assert!(rendered.starts_with("schema_version = 2"));
    // No empty `[settings]` decoration before the nested sub-tables
    // — the renderer should emit `[settings.editor]` directly. We
    // assert the negative pattern explicitly so a future toml-rs
    // upgrade that starts emitting `[settings]\n[settings.editor]`
    // is caught here.
    assert!(
        !rendered.contains("\n[settings]\n\n[settings.editor]"),
        "renderer produced an empty `[settings]` table before the\
         nested sub-tables; this is cosmetic but breaks the v2.0\
         hand-written example in the README:\n{rendered}",
    );
    // Round-trip via the canonical loader.
    let dir = temp_dir();
    let path = dir.path().join("rendered.toml");
    fs::write(&path, &rendered).unwrap();
    let loaded = Settings::load(&path).unwrap();
    assert_eq!(loaded.editor.tab_width, 2);
}

#[test]
fn validate_reports_needs_migration_on_v1_files() {
    let dir = temp_dir();
    let settings_path = dir.path().join("settings.toml");
    let connections_path = dir.path().join("connections.toml");
    fs::write(&settings_path, KITCHEN_SINK_V1_SETTINGS).unwrap();
    fs::write(&connections_path, KITCHEN_SINK_V1_CONNECTIONS).unwrap();

    let report = validate_config(&settings_path, &connections_path);
    assert!(matches!(report.settings, ValidateOutcome::NeedsMigration));
    assert!(matches!(
        report.connections,
        ValidateOutcome::NeedsMigration
    ));
}

#[test]
fn validate_reports_ok_on_v2_files() {
    let dir = temp_dir();
    let settings_path = dir.path().join("settings.toml");
    let connections_path = dir.path().join("connections.toml");
    fs::write(&settings_path, KITCHEN_SINK_V1_SETTINGS).unwrap();
    fs::write(&connections_path, KITCHEN_SINK_V1_CONNECTIONS).unwrap();
    migrate_config(
        &settings_path,
        &connections_path,
        &MigrateOptions::default(),
    )
    .unwrap();

    let report = validate_config(&settings_path, &connections_path);
    assert!(matches!(
        report.settings,
        ValidateOutcome::Ok { schema_version: 2 }
    ));
    assert!(matches!(
        report.connections,
        ValidateOutcome::Ok { schema_version: 2 }
    ));
}

#[test]
fn validate_reports_invalid_on_malformed_v2() {
    let dir = temp_dir();
    let path = dir.path().join("settings.toml");
    fs::write(
        &path,
        "schema_version = 2\n[editor]\ntab_width = not-a-number",
    )
    .unwrap();

    let report = validate_config(&path, dir.path().join("missing.toml").as_path());
    assert!(
        matches!(report.settings, ValidateOutcome::Invalid(_)),
        "expected Invalid, got {:?}",
        report.settings,
    );
}

/// T1-T4-A additive test: v1 files (no `[run]` section) load with
/// the documented defaults so unconfigured users get v1-equivalent
/// streaming behaviour.
#[test]
fn run_settings_v1_input_uses_default() {
    let dir = temp_dir();
    let path = dir.path().join("settings.toml");
    fs::write(&path, KITCHEN_SINK_V1_SETTINGS).unwrap();
    let opts = MigrateOptions::default();
    let outcome = migrate_settings(&path, &opts).unwrap();
    assert!(matches!(outcome, MigrateOutcome::Migrated { .. }));
    let settings = Settings::load(&path).unwrap();
    assert_eq!(settings.run.batch_size, 64);
    assert_eq!(settings.run.stream_flush_ms, 50);
}

/// T1-T4-A additive test: an explicit `[run]` section round-trips
/// every field through render + load.
#[test]
fn run_settings_explicit_round_trip() {
    let mut settings = Settings::default();
    settings.run.batch_size = 256;
    settings.run.stream_flush_ms = 25;
    let rendered = render_settings_v2(&settings).unwrap();
    assert!(
        rendered.contains("batch_size = 256"),
        "render dropped batch_size: {rendered}"
    );
    let dir = temp_dir();
    let path = dir.path().join("settings.toml");
    fs::write(&path, &rendered).unwrap();
    let loaded = Settings::load(&path).unwrap();
    assert_eq!(loaded.run.batch_size, 256);
    assert_eq!(loaded.run.stream_flush_ms, 25);
}

/// T1-T4-A additive test: defaults serialise as defaults.
#[test]
fn run_settings_default_is_canonical() {
    let settings = Settings::default();
    assert_eq!(settings.run.batch_size, 64);
    assert_eq!(settings.run.stream_flush_ms, 50);
}

/// T1-T4-A additive test: `narwhal config validate` returns Ok on a
/// v2 file with an explicit `[settings.run]` block. Regression
/// guard for the validate path — the previous suite only exercised
/// validate via the kitchen-sink fixture which omitted `run`.
#[test]
fn validate_accepts_explicit_run_section() {
    let mut settings = Settings::default();
    settings.run.batch_size = 128;
    settings.run.stream_flush_ms = 75;
    let rendered = render_settings_v2(&settings).unwrap();
    let dir = temp_dir();
    let settings_path = dir.path().join("settings.toml");
    fs::write(&settings_path, &rendered).unwrap();
    let connections_path = dir.path().join("connections.toml");
    let conn_file = narwhal_config::ConnectionsFile {
        schema_version: Some(2),
        ..Default::default()
    };
    fs::write(
        &connections_path,
        toml::to_string_pretty(&conn_file).unwrap(),
    )
    .unwrap();
    let report = validate_config(&settings_path, &connections_path);
    assert!(matches!(
        report.settings,
        ValidateOutcome::Ok { schema_version: 2 }
    ));
}

// ----- editor mode / mouse / preset (v2.1) -----------------------
//
// Additive tests for the editor-customization feature. None of the
// kitchen-sink assertions above are touched — v1 files that omit
// these sections must still migrate cleanly with defaults.

/// A v1 file with `keybindings.vim_mode = false` round-trips the
/// deprecated bit. The runtime translation to `editor.mode =
/// "basic"` lives in `apply_settings`; the on-disk shape must stay
/// stable so users can downgrade narwhal without their config
/// silently shape-shifting.
#[test]
#[allow(deprecated)]
fn vim_mode_false_round_trips_as_deprecated_bit() {
    let v1 = r#"
[keybindings]
vim_mode = false
"#;
    let dir = temp_dir();
    let path = dir.path().join("settings.toml");
    fs::write(&path, v1).unwrap();
    migrate_settings(&path, &MigrateOptions::default()).unwrap();
    let loaded = Settings::load(&path).unwrap();
    assert!(!loaded.keybindings.vim_mode);
    // The runtime translation is the host's job; on disk we don't
    // pre-emptively flip editor.mode.
    assert_eq!(loaded.editor.mode, narwhal_config::EditorMode::Vim);
}

/// Every new editor field round-trips through save → load with a
/// non-default value.
#[test]
fn editor_settings_full_round_trip() {
    let mut settings = Settings::default();
    settings.editor.tab_width = 8;
    settings.editor.use_spaces = false;
    settings.editor.line_numbers = false;
    settings.editor.mode = narwhal_config::EditorMode::Emacs;
    settings.editor.mouse = narwhal_config::MouseSelectionMode::ClickOnly;
    settings.editor.show_mode_indicator = false;
    settings.editor.auto_indent = false;
    settings.editor.highlight_current_line = true;
    settings.editor.scroll_off = 7;
    settings.editor.word_wrap = true;

    let dir = temp_dir();
    let path = dir.path().join("settings.toml");
    settings.save(&path).unwrap();
    let loaded = Settings::load(&path).unwrap();
    assert_eq!(loaded.editor, settings.editor);
}

/// Keybinding preset + leader round-trip.
#[test]
fn keybinding_preset_round_trip() {
    let mut settings = Settings::default();
    settings.keybindings.preset = narwhal_config::KeyPreset::Vscode;
    settings.keybindings.leader = ",".to_owned();

    let dir = temp_dir();
    let path = dir.path().join("settings.toml");
    settings.save(&path).unwrap();
    let loaded = Settings::load(&path).unwrap();
    assert_eq!(loaded.keybindings.preset, narwhal_config::KeyPreset::Vscode);
    assert_eq!(loaded.keybindings.leader, ",");
}

/// Every `EditorMode` variant accepts the lowercase wire form.
#[test]
fn editor_mode_wire_format_accepts_all_variants() {
    for (wire, expected) in [
        ("vim", narwhal_config::EditorMode::Vim),
        ("basic", narwhal_config::EditorMode::Basic),
        ("emacs", narwhal_config::EditorMode::Emacs),
    ] {
        let body = format!("schema_version = 2\n[settings.editor]\nmode = \"{wire}\"\n",);
        let dir = temp_dir();
        let path = dir.path().join("settings.toml");
        fs::write(&path, &body).unwrap();
        let loaded = Settings::load(&path).unwrap();
        assert_eq!(loaded.editor.mode, expected, "wire={wire}");
    }
}

/// Every `MouseSelectionMode` variant accepts the kebab-case wire form.
#[test]
fn mouse_mode_wire_format_accepts_all_variants() {
    for (wire, expected) in [
        ("enabled", narwhal_config::MouseSelectionMode::Enabled),
        ("click-only", narwhal_config::MouseSelectionMode::ClickOnly),
        ("disabled", narwhal_config::MouseSelectionMode::Disabled),
    ] {
        let body = format!("schema_version = 2\n[settings.editor]\nmouse = \"{wire}\"\n",);
        let dir = temp_dir();
        let path = dir.path().join("settings.toml");
        fs::write(&path, &body).unwrap();
        let loaded = Settings::load(&path).unwrap();
        assert_eq!(loaded.editor.mouse, expected, "wire={wire}");
    }
}

/// Every `KeyPreset` variant accepts the kebab-case wire form.
#[test]
fn key_preset_wire_format_accepts_all_variants() {
    for (wire, expected) in [
        ("default", narwhal_config::KeyPreset::Default),
        ("vscode", narwhal_config::KeyPreset::Vscode),
        ("datagrip", narwhal_config::KeyPreset::Datagrip),
        ("intellij", narwhal_config::KeyPreset::Intellij),
    ] {
        let body = format!("schema_version = 2\n[settings.keybindings]\npreset = \"{wire}\"\n",);
        let dir = temp_dir();
        let path = dir.path().join("settings.toml");
        fs::write(&path, &body).unwrap();
        let loaded = Settings::load(&path).unwrap();
        assert_eq!(loaded.keybindings.preset, expected, "wire={wire}");
    }
}

/// Defaults are themselves the canonical wire defaults.
#[test]
fn editor_defaults_are_canonical() {
    let s = narwhal_config::EditorSettings::default();
    assert_eq!(s.mode, narwhal_config::EditorMode::Vim);
    assert_eq!(s.mouse, narwhal_config::MouseSelectionMode::Enabled);
    assert!(s.show_mode_indicator);
    assert!(s.auto_indent);
    assert!(!s.highlight_current_line);
    assert_eq!(s.scroll_off, 3);
    assert!(!s.word_wrap);
}
