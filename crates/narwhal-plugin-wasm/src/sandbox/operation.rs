//! [`Operation`] — a concrete host-function call the enforcer
//! evaluates.
//!
//! Each variant carries the precise argument the host function
//! received so the enforcer can match against the granted
//! [`crate::capability::CapabilitySet`] without re-parsing strings on
//! the hot path.
//!
//! The variant set tracks the host's WIT surface (`host.state-get`,
//! `host.state-set`, `host.cmd`) plus the post-v2.0 surface
//! (`fs.read`, `fs.write`, `net.connect`, `env.read`) so once the
//! WIT bumps the *enforcer* is ready and downstream callers just
//! plumb the new variant through.

use std::path::PathBuf;

use crate::capability::CapabilityKind;

/// One concrete host-function call being evaluated by the
/// [`crate::sandbox::Enforcer`].
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Operation {
    /// `host.state-get(key)` / `host.state-set(key, …)`.
    StateAccess,
    /// `host.cmd(name, …)`.
    CmdInvoke { name: String },
    /// `host.fs-read(path)` (reserved — `host.fs-read` is not yet
    /// exposed on the WIT surface; T1-T5-B wires the enforcer ahead
    /// of the import so the contract is ready when the import
    /// lands).
    FsRead { path: PathBuf },
    /// `host.fs-write(path)`.
    FsWrite { path: PathBuf },
    /// `host.net-connect(host, port)`.
    NetConnect { host: String, port: u16 },
    /// `host.env-read(var)`.
    EnvRead { var: String },
}

impl Operation {
    /// The kind of capability this operation tests against. Drives
    /// the decision-cache key and the audit kind field.
    #[must_use]
    pub const fn kind(&self) -> CapabilityKind {
        match self {
            Self::StateAccess => CapabilityKind::State,
            Self::CmdInvoke { .. } => CapabilityKind::CmdInvoke,
            Self::FsRead { .. } => CapabilityKind::FsRead,
            Self::FsWrite { .. } => CapabilityKind::FsWrite,
            Self::NetConnect { .. } => CapabilityKind::NetConnect,
            Self::EnvRead { .. } => CapabilityKind::EnvRead,
        }
    }

    /// Stable cache key — uniquely identifies one (kind, argument)
    /// pair so the cache doesn't conflate distinct operations.
    pub(crate) fn cache_key(&self) -> String {
        match self {
            Self::StateAccess => "state".to_owned(),
            Self::CmdInvoke { name } => format!("cmd.invoke:{name}"),
            Self::FsRead { path } => format!("fs.read:{}", path.display()),
            Self::FsWrite { path } => format!("fs.write:{}", path.display()),
            Self::NetConnect { host, port } => format!("net.connect:{host}:{port}"),
            Self::EnvRead { var } => format!("env.read:{var}"),
        }
    }

    /// Short structured description, used as the `operation`
    /// field of the audit log.
    pub(crate) fn describe(&self) -> String {
        self.cache_key()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_keys_are_unique_per_argument() {
        let a = Operation::FsRead {
            path: PathBuf::from("/etc/a"),
        };
        let b = Operation::FsRead {
            path: PathBuf::from("/etc/b"),
        };
        assert_ne!(a.cache_key(), b.cache_key());
    }

    #[test]
    fn cache_keys_disambiguate_kinds() {
        let read = Operation::FsRead {
            path: PathBuf::from("/etc"),
        };
        let write = Operation::FsWrite {
            path: PathBuf::from("/etc"),
        };
        assert_ne!(read.cache_key(), write.cache_key());
    }

    #[test]
    fn kind_projection() {
        assert_eq!(Operation::StateAccess.kind(), CapabilityKind::State);
        assert_eq!(
            Operation::CmdInvoke { name: "run".into() }.kind(),
            CapabilityKind::CmdInvoke,
        );
    }
}
