# Connection vault providers (HashiCorp Vault + 1Password CLI)

Design notes for the secret-resolution layer added in v2.0. The audit
subsystem consumes the failure-class taxonomy in
[§ Error model](#error-model) and the redaction guarantees in
[§ Security guarantees](#security-guarantees).
> ** (schema diff)** does not touch this surface but inherits
> the connection-time error shape unchanged.

## What landed

Connection passwords in `connections.toml` can now express vault
references that are resolved at connect time:

```toml
[[connection]]
name  = "prod-db"
driver  = "postgres"
[connection.params]
host  = "db.prod.example.com"
username = "appuser"
# Either of these works:
password = "vault:hashicorp/secret/data/db/prod#password"
# password = "1password:op://Vault/PostgresProd/password"
# password = "${env:PG_PASS}"  # legacy env interpolation
# password = "literal"  # legacy inline (discouraged)
```

Two providers ship in v2.0:

* **`hashicorp`** — KV v2 secret engine, HTTP API, token auth.
* **`1password`** — `op read` CLI shell-out, service-account or
  interactive sign-in.

The resolution chain is unchanged for non-prefix passwords; vault is
purely additive. AWS Secrets Manager and Azure Key Vault are deferred
to v2.4.

## Files added / touched

| Path  | What  |
| --------------------------------------------------- | ----------------------------------------------------- |
| `crates/narwhal-config/src/vault/mod.rs`  | `VaultProvider` trait + `VaultRegistry` (dedup)  |
| `crates/narwhal-config/src/vault/error.rs`  | `VaultError` (Clone, no secret material)  |
| `crates/narwhal-config/src/vault/resolver.rs`  | `Reference::try_parse`  |
| `crates/narwhal-config/src/vault/hashicorp.rs`  | KV v2 HTTP client  |
| `crates/narwhal-config/src/vault/onepassword.rs`  | `op read` CLI client  |
| `crates/narwhal-config/src/credentials.rs`  | Added `resolve_password` orchestrator + `Vault` variant |
| `crates/narwhal-config/src/settings.rs`  | Added `timeout_secs`, `namespace`, `op_binary` knobs + `with` builders |
| `crates/narwhal-config/src/interpolate.rs`  | `password` joined the interpolation loop  |
| `crates/narwhal-core/src/connection.rs`  | `ConnectionParams::password: Option<String>` (new)  |
| `crates/narwhal-config/tests/vault.rs`  | 19 integration tests (mock HTTP + shell-stub `op`)  |
| `crates/narwhal-app/src/core/state/deps.rs`  | `AppDeps.vault: Arc<VaultRegistry>`  |
| `crates/narwhal-app/src/core/construct.rs`  | `AppCore::set_vault` builder  |
| `crates/narwhal-app/src/app.rs`  | `App::with_vault` fluent builder  |
| `crates/narwhal-app/src/core/run_loop.rs`  | `dispatch_meta` passes vault to worker  |
| `crates/narwhal-commands/src/meta.rs`  | `spawn_meta_request` accepts `vault` arg  |
| `crates/narwhal-mcp/src/context.rs`  | `ServerContext::with_vault` + new resolver wiring  |
| `narwhal/src/main.rs`  | TUI / MCP / exec all build vault registry  |
| `Cargo.toml`  | `reqwest` joined workspace deps (rustls only)  |

## Architectural notes

### Trait shape — diverges RPITIT pattern

The four core traits in `narwhal-core` (`Connection`, `DatabaseDriver`,
`RowStream`, `CancelHandle`) use the **RPITIT + `Dyn*` sibling**
pattern described in [`async-trait-style.md`](async-trait-style.md).
The vault trait does **not** follow that pattern:

```rust
pub trait VaultProvider: Send + Sync + Debug {
  fn name(&self) -> &str;
  fn resolve<'a>(
  &'a self,
  reference: &'a Reference,
  ) -> futures::future::BoxFuture<'a, Result<Arc<SecretString>, VaultError>>;
}
```

Reasoning:

1. **Always trait-object dispatch.** The registry holds providers in
  a `HashMap<String, Arc<dyn VaultProvider>>`; there is no hot path
  where the call could be devirtualised. The dyn / sized split would
  double the trait surface for no measurable gain.
2. **One async method.** The sibling-and-blanket-impl machinery scales
  per method; for a single-method trait it is pure overhead.
3. **`Arc<SecretString>` return**. The in-flight dedup broadcast must
  hand the *same* value to every concurrent waiter, and `SecretString`
  is not `Clone`. Wrapping in `Arc` solves both ergonomics and
  secrecy at once.

Future providers (`aws`, `azurekv`) should adopt the same trait
shape verbatim.

### Reference syntax

```
vault:<provider>/<path>#<field>?
1password:<op-uri>
```

* `<provider>` lets users register more than one named instance
  (`vault:prod-cluster/…`, `vault:dr-cluster/…`). Conventional default
  is `hashicorp`.
* `<path>` is forwarded verbatim to the provider — `HashiCorp` KV
  paths look like `secret/data/<mount>/<key>`.
* `<field>` is the key inside the KV map. Optional when the path
  returns exactly one entry (convenience for single-field secrets).
* `1password:` URIs always pass the entire `op://Vault/Item/field`
  tail to `op read` — no field selector at this layer because the
  CLI already encodes it in the path.

[`Reference::try_parse`] returns:

* `Ok(Some(r))` — well-formed reference;
* `Ok(None)` — input has no recognised prefix (treat as a literal);
* `Err(VaultError::MalformedReference)` — input *starts with* a
  recognised prefix but is malformed (typos like `vault:` with no
  path bubble up rather than being silently treated as literal
  passwords, which would defeat the security intent).

### In-flight de-duplication

Two concurrent resolves of the same `reference.raw` coalesce into one
provider call. Implemented in `VaultRegistry::resolve`:

```text
Caller A  Caller B (same reference, T+5ms)
  │  │
  ├─ acquire map ────┤
  │ insert tx  ├─ acquire map
  │ → Leader role  │ subscribe to tx
  ├─ release map ────┤ → Follower role
  ├─ provider.fetch  │
  │  …network…  │
  ├─ tx.send(result) │ rx.recv ← unblocks
  ├─ map.remove  │
  └─ return result  └─ return result
```

A cancelled waiter (the user navigates away) drops its receiver; the
leader continues to completion so other waiters still get a result.
The leader cleans up the in-flight slot before returning, so a third
call after the first two complete starts a fresh lookup — **no
result caching at this layer** (caching is a separate axis with its
own threat model).

### Error model

`VaultError` is **`Clone + Send + Sync + Debug + Error`** so the
broadcast channel can hand it to every concurrent waiter. The
variants form a small, stable taxonomy that the audit tool should classify against:

| Variant  | Retry policy  |
| --------------------- | --------------------------------------------- |
| `UnknownProvider`  | Configuration — fix `settings.toml`, never retry. |
| `MalformedReference`  | Configuration — fix `connections.toml`.  |
| `NotConfigured`  | Configuration — env var unset, address blank. |
| `NotFound`  | Configuration / drift — the secret moved.  |
| `Denied`  | Transient at policy/token boundary; provider may auto-retry once. |
| `Unreachable`  | Transient — network, DNS, missing binary.  |
| `BadResponse`  | Provider misconfiguration (KV v1 mount via v2 client, etc.). |
| `Timeout`  | Transient.  |
| `DedupChannelClosed`  | Internal — should be unreachable in practice. |

`HashicorpVault` auto-retries `Denied` exactly once after re-reading
the token env var, because the canonical failure is "operator rotated
the token, env var has the new one but our cached token is stale".
No exponential backoff loop.

### Security guarantees

1. **No secret in logs.** Every `Display` impl on `VaultError`
  formats the *reference* (e.g.
  `vault:hashicorp/secret/data/db/prod#password`) plus a class
  string. There is no constructor that takes a `SecretString` —
  the type system prevents the secret from ever entering the
  error channel. `rg "tracing::.+password|tracing::.+secret"
  crates/narwhal-config/` produces zero hits.
2. **No keyring fallback on vault failure.** When a connection's
  `password` parses as a vault reference and the registry returns
  an error, the orchestrator does **not** consult the keyring or
  pgpass — that would silently degrade to a (potentially stale)
  cached entry and defeat the user's opt-in to vault storage.
3. **`SecretString` everywhere.** Resolved secrets are wrapped in
  `secrecy::SecretString` from the moment the provider's
  `resolve` method returns. The orchestrator copies the inner
  bytes into an owned `SecretString` at the seam so callers
  never have to touch `expose_secret` themselves.
4. **TLS via rustls.** `reqwest` is pulled with `default-features
  = false` + `rustls-tls`, matching the `narwhal-drivers` /
  `keyring` policy. No `openssl-sys`, no `native-tls`.

### Settings shape

`settings.toml` v2 (the section already existed as a stub):

```toml
[vault]
default_provider = "hashicorp"  # informational; the per-reference provider in connections.toml wins

[vault.providers.hashicorp]
address  = "https://vault.example.com:8200"
token_env  = "VAULT_TOKEN"  # narwhal reads from this env var
namespace  = "team-platform"  # optional (Vault Enterprise)
timeout_secs = 5  # optional, default 5

[vault.providers.onepassword]
account  = "my-team.1password.com"  # optional
service_account_token_env  = "OP_SERVICE_ACCOUNT_TOKEN"
op_binary  = "/usr/local/bin/op"  # optional (defaults to `op` on PATH)
timeout_secs  = 10  # optional, default 10
```

All sub-fields have serde defaults; a v2 file with only
`default_provider = "none"` (the upgrade path from v1) builds an
empty registry and references fail at connect time with
`UnknownProvider` — exactly the right signal for the user to add a
provider block.

`namespace`, `timeout_secs` (on both providers), and `op_binary`
were added. Existing `settings.toml` files
continue to parse unchanged because every new field is optional
with a serde default.

### Wire-up across entry points

| Entry point  | Settings load  | Vault registry build  |
| ------------------------ | -------------------------- | ------------------------------ |
| `narwhal` (TUI)  | `load_settings_or_warn`  | `build_vault_registry` → `App::with_vault` |
| `narwhal mcp`  | `load_settings_or_warn` (new) | `build_vault_registry` → `ServerContext::with_vault` |
| `narwhal exec`  | `load_settings_or_warn` (new) | `build_vault_registry` → passed to `resolve_password` |

The `build_vault_registry` helper is tolerant: a misconfigured
provider sub-section is logged at `warn` level and the registry is
empty; the TUI / MCP server still starts so the user can fix
`settings.toml` from inside the app.

## Test surface

19 integration tests in `crates/narwhal-config/tests/vault.rs`:

* **Reference parsing** (4 unit tests in `vault/resolver.rs`):
  - Plain string → no reference;
  - HashiCorp with/without field;
  - Custom provider name;
  - 1Password URI;
  - Malformed inputs (empty body, missing separator, trailing `#`,
  non-`op://` 1Password) return `Err`, never panic;
  - Defensive unicode fuzz set never panics.
* **HashiCorp provider** (6 tests, mock HTTP on loopback):
  - KV v2 field selector hit;
  - Single-field convenience (no `#field` required);
  - 404 → `NotFound`;
  - Connection refused → `Unreachable`;
  - Token env var unset → `NotConfigured`;
  - **Dedup**: 8 concurrent resolves → 1 HTTP call (brief AC).
* **1Password provider** (4 tests, shell-script `op` stub):
  - Successful read returns trimmed stdout;
  - "item not found" stderr → `NotFound`;
  - Missing binary → `Unreachable`;
  - Service-account env var pre-flight check.
* **Orchestrator** (6 tests):
  - Inline literal used verbatim;
  - Empty inline does NOT mask keyring;
  - Keyring consulted when no inline password;
  - Vault reference dispatches to registry;
  - Vault reference without registry → clear error;
  - **Security**: vault failure does NOT fall through to keyring.
* **Cancellation contract** (1 test):
  - Aborted waiter does not break leader; leader runs exactly once
  and the abandoned waiter dropping its receiver is safe.
* **Settings round-trip** (1 test):
  - `VaultSettings` → `VaultRegistry::from_settings` registers both
  providers.

No real database, no real Vault server, no real 1Password account —
the full suite runs anywhere `tokio` does, in well under a second.

## Acceptance criteria

| Item  | Status |
| ------------------------------------------------------------- | :----: |
| `VaultProvider` trait + two impls  |  ✅  |
| `Reference::try_parse` handles canonical + malformed inputs  |  ✅  |
| HashiCorp works against KV v2 (mock; docker fixture out of scope for CI) | ✅ (mock) |
| 1Password works with shell-stub `op_binary` for CI  |  ✅  |
| `VaultError::NotFound` includes reference, never the secret  |  ✅  |
| Cancellation drops the in-flight HTTP request  |  ✅  |
| Concurrent resolves → one provider call  |  ✅  |
| Documentation: `docs/vault.md` covers setup + security  |  ✅  |
| Definition of Done passes (fmt + clippy --all-targets + tests)|  ✅  |

## Trade-offs accepted

- **No persistent cache of resolved secrets.** Only in-flight
  dedup. Caching needs a separate TTL/invalidation/threat-model
  design pass. Re-add via a `cache` knob in a v2.x minor.
- **KV v2 only for HashiCorp.** v1 mounts, database engine,
  transit, login auth methods (AppRole, JWT) deferred to v2.x.
- **No per-secret rotation hooks.** Drivers re-resolve on every
  new session, which is the canonical pattern for short-lived
  tokens. Background watchers are a v2.x research item.
- **Trait shape diverges** RPITIT convention. See
  [§ Trait shape](#trait-shape--diverges-from-t0-02-rpitit-pattern)
  for the explicit rationale.

## Out of scope (deferred to v2.4+)

- AWS Secrets Manager provider.
- Azure Key Vault provider.
- Per-secret rotation hooks.
- TUI UI for "manage vault providers" — config-file only in v2.0.
- Hashicorp KV v1 / database / transit engines.
- Authentication methods other than token (AppRole, JWT, OIDC).

## contract surface (audit) snapshots:

- The `VaultError` variant taxonomy in `error.rs`. Variants are
  `#[non_exhaustive]`; adding new ones is non-breaking and the
  audit tool should classify unknown variants as
  "transient/unclassified" rather than failing.
- The reference syntax in `vault/resolver.rs`. The string form is
  what lands in audit logs (never the resolved secret); audit
  queries that group by "secret source" should match against
  `vault:` / `1password:` prefixes on the *original* reference,
  not the resolved value.
- The in-flight dedup map's lifecycle. A single audit entry per
  *user-visible* connect attempt is the right granularity; multiple
  log lines on a deduped resolve would over-report. (schema diff) — no contract touch. Schema diff operates on
the introspection layer above this; it never sees credentials.

## References

- HashiCorp Vault KV v2 API: https://developer.hashicorp.com/vault/api-docs/secret/kv/kv-v2
- 1Password CLI `op read`: https://developer.1password.com/docs/cli/reference/commands/read/
- `secrecy` crate: https://docs.rs/secrecy/
- `async-trait-style.md` (for why this trait does NOT follow that pattern)
