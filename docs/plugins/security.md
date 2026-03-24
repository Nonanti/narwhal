# Plugin security — threat model and capability list

> Status: **v2.0**. Lands with T1-T5-B (WASM sandbox v2).
> Companion to [`docs/plugins/wasm.md`](./wasm.md) (SDK walkthrough)
> and [`docs/dev/t1-t5-b-sandbox.md`](../dev/t1-t5-b-sandbox.md)
> (host-side implementation notes).

## TL;DR for plugin authors

1. Declare every capability you need in `plugin.toml`. Be specific:
   `fs.read:/etc/my-plugin` is safer than `fs-read`.
2. Expect every denied call to return a wasmtime trap (for writes)
   or `None` (for reads). Wrap them in graceful fallbacks.
3. Targets the host can serve are visible to the user via the
   `[plugins.wasm]` settings section + the operator's per-plugin
   grants list. You do not get to enable yourself.

## TL;DR for operators

1. Default-deny. The shipped settings refuse every FS/net/env
   capability; you opt in per-plugin.
2. Prefer the **fine** grants list (`[[plugins.grants]]`) over the
   coarse `allow_fs_read = true` flag.
3. Audit log emits under target `narwhal::plugin::audit`. Filter
   your `tracing-subscriber` config on it to surface denials.

## Threat model

### What the WASM sandbox protects against

* **Curious-but-bounded plugins.** A plugin that snoops the
  filesystem outside its declared scope is refused. Path traversal
  via `..` is rejected at parse time *and* at every per-call
  query.
* **Resource exhaustion.** Each plugin runs inside its own
  `wasmtime::Store` with hard limits on memory (64 MiB), fuel
  (~100 M instructions per call), and KV bytes (256 KiB). Going
  past any of them traps the guest.
* **Side-channel probing of host KV.** Denied reads return `None`
  uniformly — a plugin without `state` cannot tell whether a key
  exists.
* **Cumulative log spam from denials.** The audit cache
  short-circuits repeated denials so a noisy plugin can't flood
  the operator's log.
* **Unsigned plugin code.** Out of scope for v2.0 (signing lands
  in v2.3); operators are expected to install only trusted
  components today.

### What the WASM sandbox does **not** protect against

* **Symlink races.** The enforcer does not call
  `std::fs::canonicalize` on plugin-supplied paths — the syscall
  is racy (the plugin can swap the link between resolve and use)
  and would leak host directory structure through error messages.
  Operators arranging a writable symlink that points into a denied
  area have already lost. Document the directory layout you trust.
* **Side channels via timing.** A plugin with `cmd` access can
  observe wall-clock time and infer host state. Out of scope for
  v2.0.
* **Wasmtime / cranelift CVEs.** We pin a known-good wasmtime
  version in the workspace; CVE response is "bump the version and
  ship a point release."
* **Lua plugins.** The Lua track is the *honest-author-trusted*
  path — sandboxing Lua sufficiently to defend against an
  adversarial plugin is famously incomplete. See
  [the Lua track](#lua-plugins-honest-author-trusted-only) below.

## Capability reference

Each manifest token authorises one operation. The host's grants
list intersects the manifest's requests; the runtime enforces the
intersection on every host call.

| Token form                          | What it authorises                                  |
| ----------------------------------- | --------------------------------------------------- |
| `state`                             | Per-plugin KV via `host.state-get`/`host.state-set`. KV is namespaced to the plugin — one plugin can't read another's keys. |
| `cmd`                               | **Broad** `host.cmd` — any narwhal `:` command. Legacy; prefer the explicit form. |
| `cmd.invoke:<name>`                 | Exactly the named `:` command via `host.cmd`. |
| `fs.read:<absolute-path-prefix>`    | Read access to files under the prefix (component-prefix match). |
| `fs.write:<absolute-path-prefix>`   | Write access. Does **not** imply read. |
| `net.connect:<host>`                | TCP connect to any port on the host. |
| `net.connect:<host>:<port>`         | TCP connect to the specific port. |
| `env.read:<VAR>`                    | Read the named environment variable. |
| `env.read:*`                        | Wildcard env read (== legacy bare `env`). |

### Path matching rules

* Paths must be **absolute** at parse time. Relative paths are
  rejected.
* Paths must be **lexically normalised**. `..` segments are
  rejected at parse time *and* at every per-call query.
* Matching is **component-prefix**, not byte-prefix. `fs.read:/etc`
  authorises `/etc/passwd` but **not** `/etcd-data/x`.
* No `canonicalize` syscall is performed. Symlinks are part of the
  trusted directory layout.

### Net matching rules

* Host comparison is **case-insensitive**.
* Wildcard host `*` is the legacy bare `net` semantics.
* Wildcard port (`net.connect:host`) authorises any port on the
  host.

### Cmd matching rules

* `cmd.invoke:<name>` matches **only** that command name. The
  enforcer requires exact match on the name parameter the plugin
  passes to `host.cmd`.
* `cmd` is a broad allow-list — every command name accepted.
  Operators wanting tight control should remove `cmd` from grants
  and add `cmd.invoke:<name>` per command instead.

## Settings layout

Two layers govern what a plugin can request:

### Coarse — `[plugins.wasm]`

The historical T1-T5-A shape. Bool flags. Refuse-all by default.

```toml
[plugins.wasm]
enabled       = true
allow_fs_read = true       # gates fs.read:* manifests
allow_fs_write = false     # gates fs.write:* manifests
allow_net     = false      # gates net.connect:* manifests
allow_env     = false      # gates env.read:* manifests
```

When a coarse flag is `false`, **no** manifest declaring that
capability kind loads — the manifest is refused at parse time with
a `CapabilityDenied` error.

### Fine — `[[plugins.grants]]` (parsed by `narwhal-app`)

```toml
[[plugins.grants]]
plugin       = "fmt-helper"
capabilities = [
    "fs.read:${config}/plugins/fmt-helper/",
    "cmd.invoke:fmt",
]
```

The fine grants list narrows the coarse flag's "widest scope" to
explicit allow-lists. The runtime intersects manifest ∩ coarse ∩
fine at load time; the per-call enforcer guards against the
intersection.

Embedders that don't supply a fine grants list inherit
[`RuntimeConfig::grants_from_settings`] — the coarse flags expand
to the widest scope of each kind (`allow_fs_read=true` →
`fs.read:/`).

## Lua plugins — honest-author-trusted only

Lua plugins **do not** participate in the capability model. The
`mlua` runtime carries the historical assumption that plugin
authors are honest. Removing `os.execute`, restricting `io.open`,
and similar measures provide *some* defence-in-depth, but a
determined adversary can usually find an escape (debug hooks,
weak-table tricks, metatable abuse).

For v2.0 the documented stance is:

> Treat the WASM track as the strong-isolation path. Install
> untrusted code as a WASM component. The Lua track exists for
> the install base of historical plugins and assumes operators
> have vetted the source.

The Lua bridge will tighten its stdlib exposure in a follow-up
(T1-T5-B sibling task), but the policy boundary stays the same:
WASM is sandboxed, Lua is trusted.

## Operator playbook

### Surfacing the denial audit log

```toml
# tracing-subscriber config snippet
[targets]
narwhal::plugin::audit = "warn"   # emit every denial
```

Every denial carries:

```text
plugin    = "fmt-helper"
kind      = "fs.read"
operation = "fs.read:/etc/passwd"
reason    = "no matching fs grant"
audit_id  = 42                       # per-process monotonic
```

The cache short-circuits repeated denials on the same operation —
**only the first** call emits an audit event; subsequent identical
denials reference the original `audit_id` in the trap message.

### Reading an audit id

When a plugin reports "operation denied with audit-42", grep the
log file for `audit_id=42` to find the structured record. The id
is per-process; restart resets the counter.

### Tightening a runaway plugin

1. Identify the noisy operation from the audit log.
2. Remove the matching `[[plugins.grants]]` entry — or narrow the
   scope (`fs.read:/etc` → `fs.read:/etc/my-plugin`).
3. Restart the host. Grants are loaded once at startup;
   hot-reload is out of scope for v2.0.

## Migration from T1-T5-A manifests

Manifests written against T1-T5-A use the bare unit tokens
(`state`, `cmd`, `fs-read`, `fs-write`, `net`, `env`). These keep
loading — the parser maps each to the widest scope of its kind:

| Legacy bare token | v2.0 equivalent       |
| ----------------- | --------------------- |
| `state`           | `state`               |
| `cmd`             | `cmd`                 |
| `fs-read`         | `fs.read:/`           |
| `fs-write`        | `fs.write:/`          |
| `net`             | `net.connect:*`       |
| `env`             | `env.read:*`          |

Migrating to the explicit form is **strongly recommended** — the
legacy tokens grant the *widest possible* scope of their kind, and
a typo in a new manifest is no longer recoverable to a "did you
mean…" hint. Plugin SDK templates ship with the explicit form.

## Reporting a sandbox escape

Sandbox escapes (plugin observes behaviour beyond what its
effective set authorises) are security-relevant. Open a private
issue tagged `security`; the
[narwhal SECURITY.md](../../SECURITY.md) carries the disclosure
contact. Do not file public PRs that demonstrate the escape.
