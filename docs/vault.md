# Secret vault (v2.0+)

narwhal v2.0 can resolve database connection passwords against an
external secrets manager so you never have to put plaintext into
`connections.toml`. Two providers ship out of the box:

- **HashiCorp Vault** — KV v2 secret engine over the HTTP API
- **1Password** — via the `op` CLI

This page covers setup, the reference syntax, the resolution order
and the security guarantees. The developer-facing contract — trait
shape, error taxonomy, in-flight dedup — lives in
[`docs/dev/vault-providers.md`](dev/vault-providers.md).

## TL;DR

```toml
# connections.toml
[[connection]]
name   = "prod-db"
driver = "postgres"
[connection.params]
host     = "db.prod.example.com"
username = "appuser"
password = "vault:hashicorp/secret/data/db/prod#password"
```

```toml
# settings.toml
[vault.providers.hashicorp]
address   = "https://vault.example.com:8200"
token_env = "VAULT_TOKEN"
```

```bash
export VAULT_TOKEN=$(vault print token)
narwhal
```

Open `prod-db` — the password is fetched from Vault at connect time.
The credential is never written to disk and the keyring is bypassed.

## Reference syntax

```
vault:<provider>/<path>#<field>
1password:op://<vault>/<item>/<field>
```

| Form                                                | Resolves to                          |
| --------------------------------------------------- | ------------------------------------ |
| `vault:hashicorp/secret/data/db/prod#password`      | KV v2 read, field `password`         |
| `vault:hashicorp/secret/data/db/prod`               | KV v2 read; auto-pick when only one field |
| `vault:prod-cluster/secret/data/db#password`        | Named Hashicorp instance `prod-cluster` |
| `1password:op://Engineering/PostgresProd/password`  | `op read 'op://Engineering/…'`       |
| `${env:PG_PASS}`                                    | Legacy env-var indirection (still works) |
| `inline-text-password`                              | Treated as a literal (discouraged)   |
| _(field omitted)_                                   | Keyring is consulted by `connection.id` |

The first path segment of a `vault:` reference is the **provider
name** registered in `settings.toml` (typically `hashicorp`). The
naming lets you point separate connections at different Vault
clusters without inventing a new top-level scheme.

## Resolution order

For each `:open` (or MCP tool / `exec` invocation), narwhal walks
this chain:

1. **`params.password` is `Some(s)` and `s` parses as a vault
   reference** → the registered provider resolves it. **No
   fallback** to keyring or pgpass on failure: a vault error is
   surfaced to the user.
2. **`params.password` is `Some(s)` and is not a reference** → `s`
   is used verbatim (after `${env:VAR}` interpolation).
3. **`params.password` is `None`** → the OS keyring is consulted
   by `connection.id`.
4. **Still nothing** → `~/.pgpass` and the driver-specific env
   vars (`PGPASSWORD`, `MYSQL_PWD`, `CLICKHOUSE_PASSWORD`) are
   tried.

The keyring and pgpass paths are the v1.x defaults; the only change
in v2.0 is that an explicit vault reference takes the highest
priority and **never silently degrades** to a stale cached value.

## HashiCorp Vault setup

```toml
[vault.providers.hashicorp]
address      = "https://vault.example.com:8200"
token_env    = "VAULT_TOKEN"

# Optional:
namespace    = "team-platform"        # Vault Enterprise namespace
timeout_secs = 5                      # default 5 seconds
```

| Field          | Required | Notes |
| -------------- | -------- | ----- |
| `address`      | yes      | Base URL. `${env:VAULT_ADDR}` interpolation supported. |
| `token_env`    | yes      | **Name** of the env var holding the token. Never the token itself. |
| `namespace`    | no       | Sent as `X-Vault-Namespace`. Vault Enterprise only. |
| `timeout_secs` | no       | Per-call HTTP timeout. Default 5. |

The token lifecycle is entirely in your hands. narwhal:

* reads `$VAULT_TOKEN` on every resolve;
* retries exactly once on HTTP 403 (re-reading the env var each
  time, so renewing the token externally lets the next request
  succeed);
* never persists the token anywhere.

### Worked example

```toml
# settings.toml
[vault.providers.hashicorp]
address   = "https://vault.example.com:8200"
token_env = "VAULT_TOKEN"
```

```toml
# connections.toml
[[connection]]
name   = "prod-db"
driver = "postgres"
[connection.params]
host     = "db.prod.example.com"
username = "appuser"
password = "vault:hashicorp/secret/data/db/prod#password"
```

```bash
export VAULT_TOKEN=$(vault login -method=oidc -token-only)
narwhal
```

### Multiple Vault clusters

Register more than one named provider by writing a separate
`[vault.providers.<name>]` block. The provider name is the first
path segment of every reference that should use it:

```toml
[vault.providers.prod-cluster]
address   = "https://vault.prod.example.com:8200"
token_env = "PROD_VAULT_TOKEN"

[vault.providers.dr-cluster]
address   = "https://vault.dr.example.com:8200"
token_env = "DR_VAULT_TOKEN"
```

```toml
password = "vault:prod-cluster/secret/data/db/prod#password"
password = "vault:dr-cluster/secret/data/db/dr#password"
```

> **Note**: v2.0 ships only the canonical `hashicorp` provider
> auto-registration from `settings.toml`. Multi-cluster support is
> on the v2.x roadmap; the reference syntax is already in place
> so configs written today will keep working.

## 1Password setup

```toml
[vault.providers.onepassword]
account                   = "my-team.1password.com"
service_account_token_env = "OP_SERVICE_ACCOUNT_TOKEN"

# Optional:
op_binary    = "/usr/local/bin/op"   # defaults to `op` on PATH
timeout_secs = 10                     # default 10 seconds
```

| Field                       | Required | Notes |
| --------------------------- | -------- | ----- |
| `account`                   | no       | `--account` arg to `op read`. Useful when `op` knows multiple accounts. |
| `service_account_token_env` | no       | For unattended use; pre-flight check fails fast if unset. |
| `op_binary`                 | no       | Path override (test stubs, sandboxes). Defaults to `op`. |
| `timeout_secs`              | no       | Per-call timeout. Default 10 (CLI cold-start dominates). |

Either flow works:

- **Interactive (developer laptop)**: `op signin` has already been
  run. Don't set `service_account_token_env`; narwhal invokes `op
  read` and lets the CLI handle session lookup.
- **Unattended (CI, server)**: set
  `OP_SERVICE_ACCOUNT_TOKEN` (or whatever env var
  `service_account_token_env` names) before launching narwhal.

```bash
export OP_SERVICE_ACCOUNT_TOKEN=ops_...
narwhal mcp
```

### Worked example

```toml
[vault.providers.onepassword]
service_account_token_env = "OP_SERVICE_ACCOUNT_TOKEN"
```

```toml
[[connection]]
name   = "prod-db"
driver = "postgres"
[connection.params]
host     = "db.prod.example.com"
username = "appuser"
password = "1password:op://Engineering/PostgresProd/password"
```

## Mixing vault and legacy resolution

You can mix references and legacy resolution on a per-connection
basis. Common patterns:

```toml
# Prod: enforce vault.
[[connection]]
name   = "prod-db"
[connection.params]
password = "vault:hashicorp/secret/data/db/prod#password"

# Staging: keyring (no password field, narwhal prompts and stores).
[[connection]]
name   = "staging-db"
[connection.params]
# password omitted; narwhal consults the keyring by connection.id

# Local dev: env var indirection (v1.x behaviour).
[[connection]]
name   = "local-db"
[connection.params]
password = "${env:LOCAL_DB_PASS}"
```

## Security guarantees

1. **Secrets never reach the on-disk config.** `connections.toml`
   holds references and indirection, not material. `git diff` on
   `connections.toml` shows zero secret churn.
2. **Secrets never reach the log.** Every vault error message
   includes the *reference* (so you can paste it into a vault CLI
   and reproduce) but never the resolved value. The crate-level
   audit `rg "tracing::.+password|tracing::.+secret"
   crates/narwhal-config/` returns zero hits.
3. **Vault failure does not silently degrade.** A misconfigured
   reference returns a clear error at connect time. There is no
   path that falls back to a (potentially stale) keyring entry.
4. **TLS via rustls.** No OpenSSL system dependency anywhere on
   the password-resolution path.
5. **Concurrent resolves are deduped.** Eight TUI tabs opening the
   same prod connection at once produce **one** HTTP call to
   Vault, not eight. Reduces blast radius if your audit policy is
   noisy.

## Troubleshooting

| Symptom                                                                                                       | Likely cause                                                                  |
| ------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------- |
| `vault: provider unreachable for ... reqwest…`                                                                | Bad `address`, DNS, firewall.                                                 |
| `vault: provider hashicorp not configured: env var VAULT_TOKEN is unset`                                       | Token env var not exported before launching narwhal.                          |
| `vault: access denied for vault:hashicorp/…: HTTP 403`                                                         | Token expired or lacks policy. Renew and the next attempt will succeed (one retry is automatic). |
| `vault: reference vault:hashicorp/… not found`                                                                 | Path typo or secret moved.                                                    |
| `vault: unexpected response shape for vault:hashicorp/…: KV v2 path returns 3 fields; specify one with #field` | Multi-field KV; add the `#field` selector.                                    |
| `vault: provider unreachable for 1password:…: op not found on PATH`                                            | Install the [`op` CLI](https://1password.com/downloads/command-line/).        |
| `vault: provider 1password not configured: env var OP_SERVICE_ACCOUNT_TOKEN is unset or empty`                 | Set the token env var or remove `service_account_token_env` from `settings.toml`. |
| `vault: timed out resolving … after Ns`                                                                       | Tighten `address` (proxy?) or raise `timeout_secs`.                           |

## Limitations (v2.0)

- HashiCorp **KV v2 only**. v1 mounts return a parse error.
- HashiCorp **token auth only**. AppRole / JWT / OIDC are v2.x.
- AWS Secrets Manager and Azure Key Vault are v2.4.
- No persistent cache of resolved secrets. Every new session triggers
  a fresh provider call (in-flight dedup still applies).
- No TUI UI for "manage vault providers" — `settings.toml` is the
  only configuration surface in v2.0.

## See also

- Developer notes: [`docs/dev/vault-providers.md`](dev/vault-providers.md)
- v1 → v2 migration guide: `narwhal migrate-config`
- HashiCorp KV v2 docs: <https://developer.hashicorp.com/vault/api-docs/secret/kv/kv-v2>
- 1Password CLI docs: <https://developer.1password.com/docs/cli/reference/commands/read/>
