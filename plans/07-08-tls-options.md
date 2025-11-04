# Plan 07-08 — TLS / SSL connection options

## Why

Production Postgres and MySQL refuse plain-TCP. narwhal currently
has no TLS configuration — the connection wizard doesn't expose
`sslmode`, `sslrootcert`, `sslcert`, `sslkey`, and the TOML
schema for `connections.toml` has no place to put them either.
A developer trying to connect to RDS / Cloud SQL / Aiven bounces
to another tool.

SSH tunnel is out of scope for v1.0 (see Plan 07 master) — TLS
covers ~80% of the cases.

## Scope

Add four per-connection TLS fields:

- `ssl_mode: SslMode` — `disable` | `prefer` | `require` |
                       `verify-ca` | `verify-full`
- `ssl_root_cert: Option<PathBuf>` — CA bundle
- `ssl_cert: Option<PathBuf>` — client cert
- `ssl_key: Option<PathBuf>` — client key

Wire them through:

1. `narwhal-core::Connection` struct
2. `narwhal-config` TOML schema (`[[connection]]`)
3. Connection wizard (a new "TLS" sub-page)
4. Each driver's connect path that supports TLS

Drivers:
- **postgres**   `tokio-postgres` + `tokio-postgres-rustls` (or
                 `openssl` if already in tree). Map `ssl_mode`
                 to its `SslMode` enum.
- **mysql**      `mysql_async` has native TLS support via
                 `OptsBuilder::ssl_opts`. Map similarly.
- **clickhouse** Already uses HTTP; just flip `http://` →
                 `https://` and pass the cert paths to the
                 `reqwest::Client` builder.
- **sqlite**     No TLS (file-local).
- **duckdb**     No TLS (file-local).

For sqlite/duckdb, `ssl_mode = disable` is the only valid value;
others are rejected at config load with a clear error.

Default `ssl_mode = prefer` for the network drivers so existing
configs (without TLS fields) keep working.

## Constraints

- AGENTS.md: no `unwrap()` / `expect()` in production code.
- `nix develop --command cargo fmt --all -- --check` clean.
- `nix develop --command cargo clippy --all-targets -- -D warnings` clean.
- One conventional commit, long-form.
- Don't introduce a new TLS stack — each driver crate already
  has a TLS dependency choice, reuse it.
- Backwards compatible: existing `connections.toml` files
  (without the new fields) parse and work.

## Concrete steps

### Step 1: `SslMode` enum + Connection fields

`crates/narwhal-core/src/connection.rs`:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SslMode {
    Disable,
    #[default]
    Prefer,
    Require,
    VerifyCa,
    VerifyFull,
}

pub struct Connection {
    // ... existing
    pub ssl_mode: SslMode,
    pub ssl_root_cert: Option<PathBuf>,
    pub ssl_cert: Option<PathBuf>,
    pub ssl_key: Option<PathBuf>,
}
```

### Step 2: TOML schema

`crates/narwhal-config/src/lib.rs` deserialisation adds the
fields under `[[connection]]`. Validation:
- `ssl_mode = "verify-ca"` requires `ssl_root_cert` to be Some
- `ssl_mode = "verify-full"` same
- Driver = sqlite|duckdb requires `ssl_mode = disable` (or
  unset → defaults to Disable for these)

`serde` rename rules: snake_case TOML, the enum variants
serialise as kebab-case (`"verify-full"`).

### Step 3: per-driver wiring

**postgres** — `narwhal-driver-postgres/src/lib.rs`:

```rust
fn build_tls_connector(conn: &Connection) -> Result<MakeRustlsConnect> {
    let mut roots = RootCertStore::empty();
    if let Some(path) = &conn.ssl_root_cert {
        let bytes = std::fs::read(path)?;
        let mut reader = BufReader::new(&bytes[..]);
        let certs = rustls_pemfile::certs(&mut reader)?;
        for c in certs { roots.add(&Certificate(c))?; }
    } else {
        roots.add_trust_anchors(/* webpki-roots */);
    }
    let config = rustls::ClientConfig::builder()
        .with_safe_defaults()
        .with_root_certificates(roots)
        .with_no_client_auth();
    Ok(MakeRustlsConnect::new(config))
}
```

Map `ssl_mode` to the postgres SSL negotiation prefix.

**mysql** — `OptsBuilder::default().ssl_opts(ssl_opts)` where
`ssl_opts` is built from the cert paths.

**clickhouse** — `reqwest::Client::builder()
.add_root_certificate(...)` per the existing reqwest stack.

### Step 4: wizard

`narwhal-app::wizard` — the existing wizard has fields for
host / port / user / pass / dbname etc. Add a new "TLS" sub-page
behind a "next" navigation that exposes the four fields. The
sub-page is skipped when the driver is sqlite/duckdb.

### Step 5: tests

`tests/tls.rs`:

1. `config_parses_ssl_fields` — round-trip a TOML with all four
   fields, assert deserialised values match.
2. `verify_ca_without_root_cert_rejects` — TOML with
   ssl_mode=verify-ca and no ssl_root_cert, assert parse error.
3. `sqlite_with_non_disable_rejects` — TOML with sqlite driver
   and ssl_mode=require, assert error.
4. `default_ssl_mode_prefer_for_network` — TOML missing all SSL
   fields parses, ssl_mode defaults to Prefer.

(Live-driver TLS handshake tests require a configured TLS-
enabled DB, gated behind the same feature flags as the rest of
the driver test suite — those are out of this plan's scope.)

Acceptance: +4 tests.

## Files

- `crates/narwhal-core/src/connection.rs` (SslMode, fields)
- `crates/narwhal-config/src/settings.rs` (serde + validation)
- `crates/narwhal-driver-postgres/src/lib.rs` (TLS builder)
- `crates/narwhal-driver-postgres/Cargo.toml` (rustls deps if
  not already)
- `crates/narwhal-driver-mysql/src/lib.rs` (ssl_opts wiring)
- `crates/narwhal-driver-clickhouse/src/lib.rs` (https + cert)
- `crates/narwhal-app/src/wizard.rs` (TLS sub-page)
- `crates/narwhal-config/tests/tls.rs` (new)

## Acceptance

- `nix develop --command cargo fmt --all -- --check` clean
- `nix develop --command cargo clippy --all-targets -- -D warnings` clean
- `nix develop --command cargo test --all` reports +4 from baseline
- Manual smoke against a TLS-required postgres (RDS or local
  with self-signed) succeeds with ssl_mode=require.

## Commit message template

```
feat(connection): TLS / SSL options across the network drivers

Production Postgres and MySQL refuse plain-TCP; narwhal had no
TLS configuration so connecting to RDS / Cloud SQL / Aiven was
impossible.  Add the standard four fields and wire them through
the full stack.

Connection struct (narwhal-core) gains:

- ssl_mode: disable | prefer | require | verify-ca | verify-full
            (default prefer for network drivers, disable for
            sqlite/duckdb)
- ssl_root_cert, ssl_cert, ssl_key: Option<PathBuf>

The TOML schema (connections.toml [[connection]]) deserialises
all four; verify-ca / verify-full enforce ssl_root_cert presence
at parse time, sqlite/duckdb with non-disable mode reject at
parse time.  Existing configs without the fields still load —
defaults preserve current behaviour.

Per-driver wiring uses each driver's existing TLS stack: rustls
via tokio-postgres-rustls for postgres, native mysql_async ssl_opts
for mysql, https + reqwest add_root_certificate for clickhouse.
sqlite and duckdb are file-local, no TLS path.

The connection wizard gains a "TLS" sub-page exposing the four
fields, skipped for sqlite/duckdb drivers.

SSH tunnel is intentionally not in this plan — it requires
libssh2 and platform-specific build deps that v1.0 doesn't want
to commit to.  TLS covers ~80% of the production-DB cases.

Four new tests cover round-trip parsing, verify-ca rejection
without a CA, sqlite-with-TLS rejection, and the default-Prefer
path for network drivers.
```
