//! `plugin.toml` capability-token parser.
//!
//! Tokens come in two flavours:
//!
//! * **v2.0 explicit form** — `kind.action:argument` where the
//!   argument shape is variant-specific (path prefix, host:port,
//!   env var name, command name).
//! * **T1-T5-A legacy form** — bare keywords (`fs-read`, `net`,
//!   `env`, `fs-write`) that expand to the widest scope.
//!
//! Unknown tokens are rejected here, *before* the runtime ever sees
//! the manifest, so a typo in a setting like `fs-rea` doesn't
//! silently leave a plugin under-privileged.

use thiserror::Error;

use super::Capability;
use super::scope::{EnvVar, HostPort, PathScope};

/// Errors raised while parsing a single capability token.
#[derive(Debug, Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum CapabilityParseError {
    /// The token doesn't match any documented prefix.
    #[error("unknown capability token: '{0}'")]
    Unknown(String),
    /// `cmd.invoke:` was used with an empty command name.
    #[error("'cmd.invoke' requires a command name")]
    EmptyCommandName,
    /// `fs.read:` / `fs.write:` was given a relative path.
    #[error("filesystem capability path must be absolute, got '{0}'")]
    PathNotAbsolute(String),
    /// `fs.read:` / `fs.write:` contained `..` or `.`.
    #[error("filesystem capability path '{0}' contains traversal segments")]
    PathTraversal(String),
    /// `net.connect:` was given an empty host.
    #[error("'net.connect' requires a host")]
    EmptyHost,
    /// `net.connect:host:port` port did not parse to a u16.
    #[error("'net.connect' port '{0}' is not a valid u16")]
    InvalidPort(String),
    /// `env.read:` was given an empty variable name.
    #[error("'env.read' requires a variable name")]
    EmptyEnvVar,
}

/// Parse one capability token. Whitespace around the token is
/// trimmed; everything else is structural.
pub(crate) fn parse(token: &str) -> Result<Capability, CapabilityParseError> {
    let trimmed = token.trim();
    // Legacy unit-style tokens carried over from T1-T5-A. Each maps
    // to the widest scope so an existing manifest doesn't silently
    // lose a previously-allowed permission across the upgrade.
    match trimmed {
        "state" => return Ok(Capability::State),
        "cmd" => return Ok(Capability::Cmd),
        "fs-read" => return Ok(Capability::FsRead(PathScope::root())),
        "fs-write" => return Ok(Capability::FsWrite(PathScope::root())),
        "net" => return Ok(Capability::NetConnect(HostPort::wildcard())),
        "env" => return Ok(Capability::EnvRead(EnvVar::wildcard())),
        _ => {}
    }

    let (kind, arg) = trimmed
        .split_once(':')
        .ok_or_else(|| CapabilityParseError::Unknown(trimmed.to_owned()))?;

    match kind {
        "cmd.invoke" => {
            let name = arg.trim();
            if name.is_empty() {
                return Err(CapabilityParseError::EmptyCommandName);
            }
            Ok(Capability::CmdInvoke(name.to_owned()))
        }
        "fs.read" => Ok(Capability::FsRead(PathScope::parse(arg)?)),
        "fs.write" => Ok(Capability::FsWrite(PathScope::parse(arg)?)),
        "net.connect" => Ok(Capability::NetConnect(HostPort::parse(arg)?)),
        "env.read" => Ok(Capability::EnvRead(EnvVar::parse(arg)?)),
        _ => Err(CapabilityParseError::Unknown(trimmed.to_owned())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_tokens_map_to_widest_scope() {
        assert_eq!(
            parse("fs-read").unwrap(),
            Capability::FsRead(PathScope::root())
        );
        assert_eq!(
            parse("net").unwrap(),
            Capability::NetConnect(HostPort::wildcard())
        );
        assert_eq!(
            parse("env").unwrap(),
            Capability::EnvRead(EnvVar::wildcard())
        );
        assert_eq!(parse("state").unwrap(), Capability::State);
        assert_eq!(parse("cmd").unwrap(), Capability::Cmd);
    }

    #[test]
    fn explicit_fs_read_with_path() {
        let cap = parse("fs.read:/etc").unwrap();
        match cap {
            Capability::FsRead(scope) => assert_eq!(scope.as_str(), "/etc"),
            other => panic!("expected FsRead, got {other:?}"),
        }
    }

    #[test]
    fn explicit_net_connect_with_port() {
        let cap = parse("net.connect:Example.com:443").unwrap();
        match cap {
            Capability::NetConnect(hp) => {
                assert_eq!(hp.host, "example.com");
                assert_eq!(hp.port, Some(443));
            }
            other => panic!("expected NetConnect, got {other:?}"),
        }
    }

    #[test]
    fn cmd_invoke_with_name() {
        let cap = parse("cmd.invoke:run").unwrap();
        assert_eq!(cap, Capability::CmdInvoke("run".to_owned()));
    }

    #[test]
    fn cmd_invoke_without_name_is_rejected() {
        assert_eq!(
            parse("cmd.invoke:"),
            Err(CapabilityParseError::EmptyCommandName)
        );
        assert_eq!(
            parse("cmd.invoke:   "),
            Err(CapabilityParseError::EmptyCommandName)
        );
    }

    #[test]
    fn fs_read_traversal_is_rejected() {
        assert!(matches!(
            parse("fs.read:/etc/../home"),
            Err(CapabilityParseError::PathTraversal(_))
        ));
    }

    #[test]
    fn unknown_token_is_rejected() {
        assert!(matches!(
            parse("nope"),
            Err(CapabilityParseError::Unknown(_))
        ));
        assert!(matches!(
            parse("net.listen:0.0.0.0:80"),
            Err(CapabilityParseError::Unknown(_))
        ));
    }

    #[test]
    fn round_trip_through_token_form() {
        let inputs = [
            "state",
            "cmd",
            "cmd.invoke:run",
            "fs.read:/etc",
            "fs.write:/tmp",
            "net.connect:example.com:443",
            "env.read:HOME",
        ];
        for raw in inputs {
            let cap = parse(raw).unwrap();
            let again = parse(&cap.to_token()).unwrap();
            assert_eq!(cap, again, "round-trip failed for {raw}");
        }
    }
}
