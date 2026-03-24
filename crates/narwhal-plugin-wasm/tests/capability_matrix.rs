//! Capability decision matrix — exhaustive `(requested, granted) ->
//! decision` cases.
//!
//! Lives outside the unit tests so the matrix can be read top-to-
//! bottom as a security spec rather than mixed with capability
//! type plumbing.

use narwhal_plugin_wasm::{
    Capability, CapabilitySet, EnvVar, Grants, HostPort, NoopAuditSink, Operation, PathScope,
    RecordingAuditSink, StandardEnforcer,
};
use std::path::PathBuf;
use std::sync::Arc;

use narwhal_plugin_wasm::{AuditSink, Enforcer};

fn enforcer_from(effective: CapabilitySet) -> StandardEnforcer {
    StandardEnforcer::new(
        effective,
        Arc::new(NoopAuditSink) as Arc<dyn AuditSink>,
        false,
    )
}

// --- State ------------------------------------------------------------

#[test]
fn state_allowed_when_state_granted() {
    let e = enforcer_from(CapabilitySet::from_caps([Capability::State]));
    assert!(e.check("p", &Operation::StateAccess).is_allowed());
}

#[test]
fn state_denied_when_empty() {
    let e = enforcer_from(CapabilitySet::new());
    assert!(!e.check("p", &Operation::StateAccess).is_allowed());
}

// --- Cmd --------------------------------------------------------------

#[test]
fn cmd_invoke_with_specific_grant() {
    let e = enforcer_from(CapabilitySet::from_caps([Capability::CmdInvoke(
        "run".into(),
    )]));
    assert!(
        e.check("p", &Operation::CmdInvoke { name: "run".into() })
            .is_allowed()
    );
    assert!(
        !e.check(
            "p",
            &Operation::CmdInvoke {
                name: "delete".into()
            }
        )
        .is_allowed()
    );
}

#[test]
fn cmd_invoke_with_bare_cmd_covers_any_name() {
    let e = enforcer_from(CapabilitySet::from_caps([Capability::Cmd]));
    assert!(
        e.check(
            "p",
            &Operation::CmdInvoke {
                name: "anything".into()
            }
        )
        .is_allowed()
    );
}

#[test]
fn cmd_invoke_denied_when_only_unrelated_grant() {
    let e = enforcer_from(CapabilitySet::from_caps([Capability::CmdInvoke(
        "run".into(),
    )]));
    let d = e.check(
        "p",
        &Operation::CmdInvoke {
            name: "other".into(),
        },
    );
    assert!(!d.is_allowed());
    assert!(d.audit_id().is_some(), "denials always carry an audit id");
}

// --- FsRead -----------------------------------------------------------

#[test]
fn fs_read_subdir_under_grant() {
    let e = enforcer_from(CapabilitySet::from_caps([Capability::FsRead(
        PathScope::parse("/etc").unwrap(),
    )]));
    assert!(
        e.check(
            "p",
            &Operation::FsRead {
                path: PathBuf::from("/etc/passwd")
            }
        )
        .is_allowed()
    );
}

#[test]
fn fs_read_sibling_outside_grant_denied() {
    let e = enforcer_from(CapabilitySet::from_caps([Capability::FsRead(
        PathScope::parse("/etc").unwrap(),
    )]));
    assert!(
        !e.check(
            "p",
            &Operation::FsRead {
                path: PathBuf::from("/home/x")
            }
        )
        .is_allowed()
    );
}

#[test]
fn fs_read_traversal_query_denied_even_with_root_grant() {
    let e = enforcer_from(CapabilitySet::from_caps([Capability::FsRead(
        PathScope::root(),
    )]));
    // Even with the root grant, a traversal in the *query* fails
    // — the enforcer refuses to canonicalise behind the plugin's
    // back.
    assert!(
        !e.check(
            "p",
            &Operation::FsRead {
                path: PathBuf::from("/etc/../home/.ssh")
            }
        )
        .is_allowed()
    );
}

#[test]
fn fs_read_grant_does_not_cover_fs_write() {
    let e = enforcer_from(CapabilitySet::from_caps([Capability::FsRead(
        PathScope::root(),
    )]));
    assert!(
        !e.check(
            "p",
            &Operation::FsWrite {
                path: PathBuf::from("/tmp/x")
            }
        )
        .is_allowed()
    );
}

#[test]
fn fs_read_component_prefix_not_byte_prefix() {
    // `fs.read:/etc` MUST NOT cover `/etcd-data/x`.
    let e = enforcer_from(CapabilitySet::from_caps([Capability::FsRead(
        PathScope::parse("/etc").unwrap(),
    )]));
    assert!(
        !e.check(
            "p",
            &Operation::FsRead {
                path: PathBuf::from("/etcd-data/x")
            }
        )
        .is_allowed()
    );
}

// --- NetConnect -------------------------------------------------------

#[test]
fn net_connect_specific_host_port() {
    let e = enforcer_from(CapabilitySet::from_caps([Capability::NetConnect(
        HostPort::parse("api.test:443").unwrap(),
    )]));
    assert!(
        e.check(
            "p",
            &Operation::NetConnect {
                host: "api.test".into(),
                port: 443
            }
        )
        .is_allowed()
    );
    assert!(
        !e.check(
            "p",
            &Operation::NetConnect {
                host: "api.test".into(),
                port: 80
            }
        )
        .is_allowed()
    );
    assert!(
        !e.check(
            "p",
            &Operation::NetConnect {
                host: "other.test".into(),
                port: 443
            }
        )
        .is_allowed()
    );
}

#[test]
fn net_connect_host_no_port_grants_any_port() {
    let e = enforcer_from(CapabilitySet::from_caps([Capability::NetConnect(
        HostPort::parse("api.test").unwrap(),
    )]));
    for port in [80_u16, 443, 8080] {
        assert!(
            e.check(
                "p",
                &Operation::NetConnect {
                    host: "api.test".into(),
                    port
                }
            )
            .is_allowed(),
            "port {port} should be allowed"
        );
    }
}

#[test]
fn net_connect_wildcard_host_grants_any() {
    let e = enforcer_from(CapabilitySet::from_caps([Capability::NetConnect(
        HostPort::wildcard(),
    )]));
    assert!(
        e.check(
            "p",
            &Operation::NetConnect {
                host: "anywhere".into(),
                port: 1
            }
        )
        .is_allowed()
    );
}

#[test]
fn net_connect_host_case_insensitive() {
    let e = enforcer_from(CapabilitySet::from_caps([Capability::NetConnect(
        HostPort::parse("Example.Com:443").unwrap(),
    )]));
    assert!(
        e.check(
            "p",
            &Operation::NetConnect {
                host: "EXAMPLE.com".into(),
                port: 443
            }
        )
        .is_allowed()
    );
}

// --- EnvRead ----------------------------------------------------------

#[test]
fn env_read_specific_var() {
    let e = enforcer_from(CapabilitySet::from_caps([Capability::EnvRead(
        EnvVar::parse("HOME").unwrap(),
    )]));
    assert!(
        e.check("p", &Operation::EnvRead { var: "HOME".into() })
            .is_allowed()
    );
    assert!(
        !e.check("p", &Operation::EnvRead { var: "PATH".into() })
            .is_allowed()
    );
}

#[test]
fn env_read_wildcard_grants_any() {
    let e = enforcer_from(CapabilitySet::from_caps([Capability::EnvRead(
        EnvVar::wildcard(),
    )]));
    for v in ["HOME", "PATH", "SHELL"] {
        assert!(
            e.check("p", &Operation::EnvRead { var: v.into() })
                .is_allowed(),
            "var {v} should be allowed"
        );
    }
}

// --- Audit + cache ----------------------------------------------------

#[test]
fn audit_event_carries_kind_operation_reason() {
    let audit = Arc::new(RecordingAuditSink::new());
    let e = StandardEnforcer::new(
        CapabilitySet::new(),
        audit.clone() as Arc<dyn AuditSink>,
        false,
    );
    let _ = e.check(
        "fmt-helper",
        &Operation::FsRead {
            path: PathBuf::from("/etc/passwd"),
        },
    );
    let snap = audit.snapshot();
    assert_eq!(snap.len(), 1);
    assert_eq!(snap[0].plugin, "fmt-helper");
    assert_eq!(snap[0].operation, "fs.read:/etc/passwd");
    assert!(snap[0].reason.contains("no matching"));
}

#[test]
fn cache_hit_does_not_double_audit() {
    let audit = Arc::new(RecordingAuditSink::new());
    let e = StandardEnforcer::new(
        CapabilitySet::new(),
        audit.clone() as Arc<dyn AuditSink>,
        false,
    );
    let op = Operation::FsRead {
        path: PathBuf::from("/etc/passwd"),
    };
    for _ in 0..10 {
        let _ = e.check("p", &op);
    }
    assert_eq!(audit.len(), 1, "only the first denial audits");
}

// --- Grants intersection ---------------------------------------------

#[test]
fn grants_open_all_covers_every_kind() {
    let grants = Grants::open_all();
    let req = CapabilitySet::from_caps([
        Capability::State,
        Capability::FsRead(PathScope::parse("/anywhere").unwrap()),
        Capability::NetConnect(HostPort::parse("api.test:443").unwrap()),
        Capability::EnvRead(EnvVar::parse("HOME").unwrap()),
    ]);
    assert!(grants.intersect(&req).is_ok());
}

#[test]
fn grants_deny_all_blocks_fs() {
    let grants = Grants::deny_all();
    let req = CapabilitySet::from_caps([Capability::FsRead(PathScope::parse("/etc").unwrap())]);
    let err = grants.intersect(&req).expect_err("fs.read denied");
    assert!(matches!(err, Capability::FsRead(_)));
}

#[test]
fn grants_narrower_than_request_returns_first_uncovered() {
    let grants = Grants::from_caps([Capability::FsRead(PathScope::parse("/etc").unwrap())]);
    let req = CapabilitySet::from_caps([
        Capability::FsRead(PathScope::parse("/etc/passwd").unwrap()),
        Capability::FsRead(PathScope::parse("/home").unwrap()),
    ]);
    let err = grants.intersect(&req).expect_err("home not covered");
    match err {
        Capability::FsRead(scope) => assert_eq!(scope.as_str(), "/home"),
        other => panic!("unexpected: {other:?}"),
    }
}
