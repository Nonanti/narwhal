//! SSH plumbing tests that don't require a real sshd.
//!
//! The actual tunnel spawn lives in `narwhal-core`; here we just
//! verify the wizard / URL parser / serde pipeline carries the SSH
//! block end-to-end without dropping fields.

use narwhal_app::wizard::{ConnectionWizard, WizardFieldKind, WizardFieldValue};
use narwhal_config::parse_url;
use narwhal_core::SshConfig;

fn set_field(w: &mut ConnectionWizard, kind: WizardFieldKind, value: &str) {
    let idx = w
        .fields
        .iter()
        .position(|f| f.kind == kind)
        .unwrap_or_else(|| panic!("no field of kind {kind:?} for driver {}", w.driver()));
    w.fields[idx].value = WizardFieldValue::Public(value.into());
}

/// The wizard exposes ssh_* fields for every network driver and the
/// build path packs them into `SshConfig`.
#[test]
fn wizard_emits_ssh_config_for_postgres() {
    let mut w = ConnectionWizard::new();
    w.cycle_driver(1); // postgres
    set_field(&mut w, WizardFieldKind::Name, "prod");
    set_field(&mut w, WizardFieldKind::Host, "db.internal");
    set_field(&mut w, WizardFieldKind::Database, "inventory");
    set_field(&mut w, WizardFieldKind::Username, "alice");
    set_field(&mut w, WizardFieldKind::SshHost, "jump.example.com");
    set_field(&mut w, WizardFieldKind::SshUser, "ubuntu");
    set_field(&mut w, WizardFieldKind::SshPort, "2222");
    set_field(&mut w, WizardFieldKind::SshKey, "/home/me/.ssh/jump_key");

    let built = w.build().expect("build");
    let ssh = built.config.params.ssh.expect("ssh block must be present");
    assert_eq!(ssh.host, "jump.example.com");
    assert_eq!(ssh.user, "ubuntu");
    assert_eq!(ssh.port, Some(2222));
    assert_eq!(
        ssh.key_path.as_ref().unwrap().to_string_lossy(),
        "/home/me/.ssh/jump_key"
    );
}

/// `ssh_host` without `ssh_user` is a user error — fail clearly
/// rather than producing a half-built SshConfig.
#[test]
fn wizard_rejects_half_filled_ssh_block() {
    let mut w = ConnectionWizard::new();
    w.cycle_driver(1);
    set_field(&mut w, WizardFieldKind::Name, "x");
    set_field(&mut w, WizardFieldKind::Host, "h");
    set_field(&mut w, WizardFieldKind::Database, "d");
    set_field(&mut w, WizardFieldKind::Username, "u");
    set_field(&mut w, WizardFieldKind::SshHost, "jump");
    let err = w.build().unwrap_err();
    assert!(err.contains("ssh_user"), "got: {err}");
}

/// Setting any non-host ssh field without ssh_host is also an error.
#[test]
fn wizard_rejects_ssh_user_without_host() {
    let mut w = ConnectionWizard::new();
    w.cycle_driver(1);
    set_field(&mut w, WizardFieldKind::Name, "x");
    set_field(&mut w, WizardFieldKind::Host, "h");
    set_field(&mut w, WizardFieldKind::Database, "d");
    set_field(&mut w, WizardFieldKind::Username, "u");
    set_field(&mut w, WizardFieldKind::SshUser, "stray");
    let err = w.build().unwrap_err();
    assert!(err.contains("ssh_host"), "got: {err}");
}

/// Round-trip an SshConfig through `from_config` → `build`.
#[test]
fn wizard_round_trips_existing_ssh_block() {
    use narwhal_core::{ConnectionConfig, ConnectionParams};
    use uuid::Uuid;

    let mut ssh = SshConfig::new("bastion", "deploy");
    ssh.port = Some(22);
    let original = ConnectionConfig {
        id: Uuid::new_v4(),
        name: "prod".into(),
        driver: "postgres".into(),
        params: ConnectionParams {
            host: Some("db.internal".into()),
            port: Some(5432),
            database: Some("inv".into()),
            username: Some("alice".into()),
            ssh: Some(ssh),
            ..Default::default()
        },
    };

    let w = ConnectionWizard::from_config(&original, None, Some(original.id));
    let built = w.build().expect("build");
    let rebuilt = built.config.params.ssh.expect("ssh survived round-trip");
    assert_eq!(rebuilt.host, "bastion");
    assert_eq!(rebuilt.user, "deploy");
    assert_eq!(rebuilt.port, Some(22));
}

/// The URL parser accepts `?ssh_host=…&ssh_user=…`.
#[test]
fn url_parser_picks_up_ssh_block() {
    let parsed =
        parse_url("postgres://alice@db.internal/inv?ssh_host=jump.example.com&ssh_user=ubuntu")
            .expect("parse");
    let ssh = parsed.config.params.ssh.expect("ssh block must be present");
    assert_eq!(ssh.host, "jump.example.com");
    assert_eq!(ssh.user, "ubuntu");
    assert_eq!(ssh.port, None);
}

/// `ssh_port` without a numeric value should error rather than silently
/// drop the SSH block.
#[test]
fn url_parser_rejects_invalid_ssh_port() {
    let err = parse_url("postgres://alice@db/inv?ssh_host=jump&ssh_user=u&ssh_port=not-a-number")
        .unwrap_err();
    assert!(format!("{err}").contains("port"), "got: {err}");
}

/// `ssh_host` without `ssh_user` in a URL surfaces the same error
/// shape as the wizard does.
#[test]
fn url_parser_rejects_lone_ssh_host() {
    let err = parse_url("postgres://alice@db/inv?ssh_host=jump").unwrap_err();
    let msg = format!("{err}");
    assert!(msg.contains("ssh_user"), "got: {msg}");
}

/// Without any ssh_* keys the parser leaves `params.ssh` as `None`
/// (regression guard for the no-tunnel happy path).
#[test]
fn url_parser_leaves_ssh_none_when_unspecified() {
    let parsed = parse_url("postgres://alice@db/inv").expect("parse");
    assert!(parsed.config.params.ssh.is_none());
}
