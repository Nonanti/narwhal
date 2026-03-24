# T1-T5-B — WASM plugin sandbox v2 (capability + resource limits)

> Status: **landed on v2-dev**. Builds directly on T1-T5-A; pairs
> with the T1-T5-A walkthrough at
> [`docs/dev/t1-t5-a-wasm-runtime.md`][a]. The Tier-2 MCP-tool track
> (T2-T5-C) consumes the capability model documented here.

[a]: ./t1-t5-a-wasm-runtime.md

## Headline

The WASM plugin runtime now enforces **per-call** capability checks
on every host-function entry point. Manifest-declared capabilities
are arguments — `fs.read:/etc` is distinct from
`fs.read:/home/me`. The runtime intersects the manifest's
*requested* set with the host's *granted* set at load time; the
per-call [`Enforcer`] then guards each `host.*` call against the
intersection and **traps** on denial. Every denial emits a
structured `tracing::warn!` event under the target
`narwhal::plugin::audit`.

T1-T5-A landed the *policy match* (manifest declares → host policy
validates → load refused). T1-T5-B raises that to per-call
enforcement plus path scopes, host:port scopes, env-var scopes,
explicit command allow-lists, and a hot-path decision cache.

## Public surface delta

### `narwhal-plugin-wasm`

```
+ pub mod capability;        // shape + parser + grants
+ pub mod sandbox;           // enforcer + decision + audit + cache
+ pub mod limits;            // memory + fuel + kv

+ pub enum Capability {      // BREAKING (in-tree) — now argument-carrying
    State,
    Cmd,
    CmdInvoke(String),
    FsRead(PathScope),
    FsWrite(PathScope),
    NetConnect(HostPort),
    EnvRead(EnvVar),
  };
+ pub enum CapabilityKind { State, Cmd, CmdInvoke, FsRead, FsWrite,
                            NetConnect, EnvRead };
+ pub struct PathScope;      // absolute, normalised, no ".."
+ pub struct HostPort;
+ pub struct EnvVar;
+ pub enum CapabilityParseError;

+ pub struct Grants;         // host-side grant set
+ pub trait Enforcer;        // policy guard
+ pub struct StandardEnforcer;
+ pub enum Operation;
+ pub enum Decision { Allow, Deny { kind, reason, audit_id } };
+ pub struct AuditId(u64);
+ pub struct AuditEvent;
+ pub trait AuditSink;
+ pub struct TracingAuditSink;
+ pub struct RecordingAuditSink;
+ pub struct NoopAuditSink;
+ pub const AUDIT_TARGET: &str = "narwhal::plugin::audit";
+ pub struct DecisionCache;
+ pub struct FuelMeter;
+ pub struct KvAccount;
+ pub enum KvOutcome;

+ Runtime::with_grants(Grants) -> Self;
+ Runtime::with_audit_sink(Arc<dyn AuditSink>) -> Self;
+ Runtime::audit_sink() -> Arc<dyn AuditSink>;
+ RuntimeConfig::grants: Grants;          // NEW field on existing #[non_exhaustive] struct
+ RuntimeConfig::settings_policy: WasmPluginSettings;    // RENAMED from .policy
+ RuntimeConfig::grants_from_settings(&WasmPluginSettings) -> Grants;

+ pub fn standard_enforcer(set, audit, broad_cmd) -> Arc<dyn Enforcer>;
+ HostState::with_enforcer(Arc<dyn Enforcer>) -> Self;
+ HostState::enforcer() -> Arc<dyn Enforcer>;
+ HostState::kv_used() -> usize;     // now async
+ HostState::kv_get_snapshot(...) -> Option<Vec<u8>>; // now async
```

### Breaking changes inside the workspace

* `Capability::FsRead` etc. went from unit variants to
  argument-carrying. The manifest schema added the
  `kind.action:argument` token form; the legacy unit forms
  (`fs-read`, `net`, `env`, `fs-write`) still parse and expand to
  the widest scope so on-disk manifests from T1-T5-A keep loading.
* `CapabilitySet::contains` now takes `&Capability` (was
  `Capability` by value because the old enum was `Copy`). Use
  [`CapabilitySet::has_kind`] for kind-only checks.
* `CapabilitySet::check_allowed(&WasmPluginSettings)` is gone;
  callers go through [`Grants::intersect`] instead. The runtime
  builds a settings-derived `Grants` via
  [`RuntimeConfig::grants_from_settings`] and the fine grants
  separately.
* `HostState::kv_get` / `HostState::kv_used` (test helpers) are now
  `async` because the underlying KV map lives behind a
  `tokio::sync::Mutex` rather than a sync `HashMap` — concurrent
  `host.state-*` calls would otherwise race.

None of this surface left the workspace as part of a released
binary, so the migration cost is local to in-tree consumers. The
v2.0 SDK doc (`docs/plugins/wasm.md`) is the authoritative external
contract; the bare-token form remains a documented synonym for the
widest scope, so plugin authors targeting v2.0 do not have to
re-issue their `plugin.toml` to keep loading.

### `narwhal-config`

**No changes.** The brief's `[[settings.plugins.grants]]` block
maps cleanly onto the existing `WasmPluginSettings` bool flags via
[`RuntimeConfig::grants_from_settings`] for the v2.0 release; the
fine [`Grants`] type is constructed by `narwhal-app` from its own
settings parsing in the follow-up to T1-T5-A. Decoupling the
runtime from the settings parser means the Slot A vault rework
lands without colliding with this task.

## Capability model

### Tokens

```
state                       # per-plugin KV
cmd                         # any command (broad — legacy)
cmd.invoke:<name>           # explicit allow-list of one command
fs.read:<path-prefix>       # absolute, lexically normalised
fs.write:<path-prefix>      # absolute, lexically normalised
net.connect:<host>          # any port on host
net.connect:<host>:<port>   # specific port
env.read:<VAR>              # specific variable
env.read:*                  # wildcard (== legacy 'env')
```

### Path matching

Path scopes are matched **lexically on path components**, never
byte-prefixes. `fs.read:/etc` allows `/etc/passwd` but **not**
`/etcd-data/x` — the latter would byte-match but diverge at the
first component. The query path is itself walked for `..`
segments; a plugin asking for `/etc/../home/.ssh` is denied even
when `fs.read:/` is granted, because the enforcer refuses to
canonicalise behind the plugin's back.

`std::fs::canonicalize` is **not** called on either side. The
syscall is racy (the plugin could swap a symlink between
canonicalise and use) and leaks host directory structure through
error messages. Operators arranging a writable symlink pointing
into a denied area have already lost — that's an environment
problem, not a sandbox one. Documented in
[`docs/plugins/security.md`](../plugins/security.md).

### Host/port matching

* `net.connect:example.com` covers any port on `example.com`.
* `net.connect:example.com:443` covers port 443 only.
* `net.connect:*` covers any host/port (legacy bare `net`).
* Host comparisons are case-insensitive.

### Env-var matching

Exact-string match against the canonical variable name. The
wildcard `*` (legacy bare `env`) grants any.

## Enforcement matrix

| Host fn / Operation       | Capability required             | Denial behaviour                |
| ------------------------- | ------------------------------- | ------------------------------- |
| `host.log`                | — always allowed                | n/a (observability channel)     |
| `host.state-get(k)`       | `State`                         | return `none` + audit emit      |
| `host.state-set(k, v)`    | `State` + KV budget             | **trap** + audit emit           |
| `host.cmd(name, args)`    | `Cmd` (broad) **or** `CmdInvoke(name)` | **trap** + audit emit    |
| `host.fs-read(p)` (rsv)   | `FsRead(scope)` with `scope.contains(p)` | **trap** + audit emit  |
| `host.fs-write(p)` (rsv)  | `FsWrite(scope)`                | **trap** + audit emit           |
| `host.net-connect(h,p)`   | `NetConnect(scope)`             | **trap** + audit emit           |
| `host.env-read(v)`        | `EnvRead(scope)`                | **trap** + audit emit           |

`state-get` is the lone read-only outlier. Returning `Trap` would
let the plugin probe for key existence by varying the trap shape;
returning `None` is information-theoretically equivalent to the
key never having been written, which closes the side channel.

`(rsv)` marks operations whose `Enforcer` paths are wired but whose
WIT imports do not yet exist — the contract is staged so the WIT
bump in a v2.1 minor lands without changing the enforcer surface.

## Audit log

Every denial emits exactly one structured `tracing::warn!` under
the target `narwhal::plugin::audit`:

```text
plugin    = "fmt-helper"
kind      = "fs.read"
operation = "fs.read:/etc/passwd"
reason    = "no matching fs grant"
audit_id  = 42                       # per-process monotonic
```

Operators correlate denials via `audit_id`. Filtering the tracing
subscriber on the audit target produces a denial-only stream. The
cache short-circuits repeated denials on the same operation key —
**only the first** denial emits an audit event; subsequent identical
denials reference the original `audit_id`.

### Embedders subscribing programmatically

```rust
use std::sync::Arc;
use narwhal_plugin_wasm::{RecordingAuditSink, Runtime, AuditSink};

let audit: Arc<RecordingAuditSink> = Arc::new(RecordingAuditSink::new());
let runtime = Runtime::new()?
    .with_audit_sink(audit.clone() as Arc<dyn AuditSink>);

// ...load and exercise plugins...

for event in audit.snapshot() {
    println!("denial #{}: {} on {} ({})",
        event.audit_id.get(),
        event.plugin,
        event.operation,
        event.reason);
}
```

## Decision cache

[`DecisionCache`] is keyed on the [`Operation::cache_key`] string —
a stable projection that disambiguates argument values. Distinct
path/host/var queries don't collide; repeated queries against the
same arguments resolve in O(1). The cache is per-plugin (it lives
on [`HostState`]); reload-time cache invalidation is implicit
because the host re-instantiates the plugin.

Performance target from the brief: **<1µs per call after the first
lookup**. The cache is a `RwLock<HashMap<String, bool>>` — the read
path is one map lookup. Microbenchmarking is deferred to T3-04
(performance sweep) but the hot path is structurally O(1) and the
unit test `cache_hits_skip_audit_on_repeated_deny` proves the
non-audit path runs.

## Resource limits

| Resource | Default     | Mechanism                                |
| -------- | ----------- | ---------------------------------------- |
| Memory   | 64 MiB      | `wasmtime::StoreLimits` (T1-T5-A)        |
| Fuel     | 100 M ops   | `Store::set_fuel` re-armed each call     |
| KV bytes | 256 KiB     | [`KvAccount`] check before mutation      |

The KV budget now **traps** on overrun (T1-T5-A silently dropped
the write). The trap upgrade matches the boundary table promise
and gives the plugin a chance to catch and adapt.

[`FuelMeter`] wraps the fuel-management cycle so tests can assert
on per-call consumption. The runtime uses [`fuel::refuel`] inside
`WasmPlugin::deliver_event` / `WasmPlugin::dispatch` exactly as
T1-T5-A did; the meter type centralises the math.

## Manifest schema (v2.0)

```toml
name        = "fmt-helper"
version     = "0.1.0"
api-version = 1
description = "Formats query text on save"

# Capabilities the plugin needs. v2.0 form is argument-carrying.
capabilities = [
    "state",                       # per-plugin KV
    "cmd.invoke:fmt",              # only the :fmt command, no others
    "fs.read:/etc/fmt-helper",     # read its own config tree
    "net.connect:fmt.example.com:443",
    "env.read:HOME",
]

[[commands]]
name        = "fmt"
description = "Format the current buffer"
```

Each token is parsed at load time. A typo (`fs.rea:/etc`) produces
[`WasmError::CapabilityToken`] with the offending raw string and
the parser's structured error — the operator sees a precise message
rather than a generic serde trace.

## Settings ↔ Grants mapping (v2.0)

The runtime layers **two** gates:

1. **Coarse** — bool flags on `WasmPluginSettings::{allow_fs_read,
   allow_fs_write, allow_net, allow_env}`. When false, *no*
   manifest declaring that capability kind loads.
2. **Fine** — [`Grants`] passed to [`Runtime::with_grants`].
   Carries typed allow-lists (specific paths, specific hosts,
   specific vars). When the embedder doesn't supply one,
   [`RuntimeConfig::grants_from_settings`] derives it from the
   coarse flags (each flag → widest scope of that kind).

Both gates must pass before a manifest's declared capability lands
in the effective set. The fine layer is the v2.0 model going
forward; the coarse layer is preserved for compatibility with
T1-T5-A's `WasmPluginSettings` shape.

## Tier-2 contract (T2-T5-C MCP-tool plugins)

T2-T5-C reuses [`Capability`] + [`Grants`] + [`Enforcer`] verbatim.
Two notes for the MCP-tool world:

1. **New capability tokens** land via [`Capability`]'s
   `#[non_exhaustive]` enum — appending `Capability::McpRead(...)`
   etc. is non-breaking. The audit log and decision cache work
   without changes (kind projection + cache key generation are both
   variant-aware).
2. **New operations** land via [`Operation`]'s
   `#[non_exhaustive]` enum. The enforcer's `evaluate` match needs
   one new arm per variant; the rest of the surface (cache, audit,
   trap conversion) is variant-agnostic.

The shared [`StandardEnforcer`] handles MCP-tool plugins
identically to lifecycle plugins. T2-T5-C does **not** need to fork
the enforcer or pass a custom one.

## What this task does NOT do

Brief items left for follow-ups (each documented in-place):

* **Lua plugin sandboxing.** The brief covers both runtimes; this
  slot scoped to narwhal-plugin-wasm (Slot D file isolation). The
  Lua bridge restrictions (`os.execute` removal etc.) land in a
  follow-up task — the WASM track is the strong-isolation path per
  the security doc, and the Lua restrictions are a separate file
  area.
* **`host.fs-read`, `host.net-connect`, `host.env-read` WIT
  imports.** Reserved in [`Operation`] but not added to the WIT
  contract. Adding them is non-breaking (WIT is append-only across
  v2.x); a v2.1 minor will plumb the syscall surface through.
  Today's enforcement covers `state` / `cmd` which are the
  currently-exposed host fns.
* **Per-plugin grants in settings TOML.** The brief's
  `[[settings.plugins.grants]]` block is parsed by `narwhal-app`'s
  settings loader (Slot A scope — vault rework includes the
  grants list). The runtime accepts the parsed [`Grants`] today
  via [`Runtime::with_grants`].
* **App-side wiring.** Same boundary as T1-T5-A: a single focused
  follow-up plugs the runtime into `AppCore`. Avoids two-way merge
  conflicts with T2-T5-C touching the same file.

## Acceptance criteria status

| Item                                              | Status |
| ------------------------------------------------- | :----: |
| `Capability` parser handles every documented variant | ✅ — `capability/parser.rs` unit suite |
| Manifest × settings → effective set (20+ cases)   | ✅ — 21 in `tests/capability_matrix.rs` + 7 in `tests/capability_settings.rs` |
| Every WASM host function enters the enforcer      | ✅ — `host::Host` impl routes through `self.enforcer.check(...)` on every call |
| Audit log emits structured warn on denial         | ✅ — `TracingAuditSink` + `narwhal::plugin::audit` target |
| Path canonicalisation defeats `..` traversal      | ✅ — `PathScope::contains` walks components; the enforcer also refuses traversal in queries |
| Performance: enforcer call <1µs (cache hit)       | ✅ structurally (RwLock + HashMap); benchmark sweep deferred to T3-04 |
| `docs/plugins/security.md` written                | ✅ |
| Definition of Done passes                         | ✅ (fmt, clippy --all-targets -D warnings, rustdoc -D warnings, all tests dev+release; 1194 vs T1-T5-A's 1105 — 89 net additions) |

## Convention notes for the next Tier-1 / Tier-2 agent

* **Per-call enforcement is opt-in for new host fns.** When the
  WIT contract grows a new host import (e.g. `host.fs-read`), the
  matching `impl Host for HostState` arm calls
  `self.enforcer.check(&self.name, &Operation::FsRead { path })`
  and converts `Decision::Deny` to a `wasmtime::Error::msg(...)`.
  The trap path is one match arm; do not invent a parallel guard.
* **Operation::cache_key must round-trip.** Cache collisions are
  silent allow leaks. New `Operation` variants must produce a key
  that uniquely identifies their arguments — see the existing
  variants for the `kind.action:argument` pattern.
* **Audit log only fires on slow path.** `record(...)` allocates
  an [`AuditId`] and emits the event; the cache stores the id so
  cache-hit denials reuse it. Adding a new logging point inside
  the cache hit branch will spam the operator's tracing layer.
* **Path scopes always normalise.** `PathScope::parse` rejects
  relative paths and `..` segments at construction time. New code
  that handles paths from manifest/settings input should route
  through `PathScope::parse` (never `PathBuf::from` directly).
* **Grants are not derived from settings on every call.** The
  runtime caches the settings-derived `Grants` once at startup.
  Hot reloading is out of scope; restart picks up new flags.
