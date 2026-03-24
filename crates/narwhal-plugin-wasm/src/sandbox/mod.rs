//! Capability enforcement, decision logging, and the hot-path
//! decision cache that backs them.
//!
//! ## Design at a glance
//!
//! ```text
//!  host fn entry ─► Enforcer::check(plugin, op)
//!                        │
//!                        ▼
//!             ┌──────────────────────────┐
//!             │  decision cache (RwLock) │  ← O(1) on the steady state
//!             └──────────────────────────┘
//!                        │  miss
//!                        ▼
//!             ┌──────────────────────────┐
//!             │  CapabilitySet::covers   │  ← walk the granted set
//!             └──────────────────────────┘
//!                        │
//!                        ▼
//!             ┌──────────────────────────┐
//!             │  audit emit (denial)     │  ← tracing target
//!             │                          │    narwhal::plugin::audit
//!             └──────────────────────────┘
//! ```
//!
//! The cache is keyed on `(plugin name, operation cache key)` —
//! distinct path/host queries don't collide, but repeated calls
//! against the same path short-circuit on the first probe.
//!
//! ## What this module enforces
//!
//! v0.2 enforcement coverage:
//!
//! * `host.state-{get,set}` → hard trap when `State` is missing.
//! * `host.cmd(name, …)` → hard trap unless either
//!   `Cmd` (broad) or `CmdInvoke(name)` (exact) is in the
//!   effective set.
//! * Manifest-load time check for `FsRead`/`FsWrite`/`NetConnect`/
//!   `EnvRead` against host grants. (Per-call FS/net/env enforcement
//!   piggy-backs on WASI preview-2 — wired in T1-T5-A on the WIT
//!   surface; the host-side syscall surface for them lands as a
//!   follow-up once WIT exposes the imports.)
//!
//! ## Audit trail
//!
//! Every denial emits a structured `tracing::warn!` event under the
//! target `narwhal::plugin::audit` with fields `plugin`,
//! `operation`, `kind`, `reason`, `audit_id`. The [`AuditId`]
//! is a per-process monotonic counter — operators correlating
//! logs across days reference one.

mod audit;
mod cache;
mod decision;
mod enforcer;
mod operation;

pub use audit::{
    AUDIT_TARGET, AuditEvent, AuditSink, NoopAuditSink, RecordingAuditSink, TracingAuditSink,
};
pub use cache::DecisionCache;
pub use decision::{AuditId, Decision};
pub use enforcer::{Enforcer, StandardEnforcer};
pub use operation::Operation;
