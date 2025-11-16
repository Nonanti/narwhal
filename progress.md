# Progress

## Status
In Progress

## Tasks

## Files Changed

## Notes

## Alan C Review — TLS / Cancellation / Plugin Runtime (Network + Sandbox)
**Status:** ✅ Complete  
**Output:** /tmp/review-c-network.md  
**Verdict:** ⚠️ Changes Requested

### Critical (3):
- K1: Postgres `insecure_client_config` drops mTLS silently for `ssl_mode=require` — `with_no_client_auth()` always used, `let _ = params` placeholder in prod code
- K2: ClickHouse `ssl_root_cert` loaded but bypassed by `danger_accept_invalid_certs(true)` for Prefer/Require — user gets false sense of CA pinning
- K3: `Duration::from_secs_f64` panics for `set_timeout(1e20)` — inside spawn_blocking → opaque JoinError

### High (4):
- Y1: MySQL partial mTLS (ssl_cert but no ssl_key) silently skipped — no validation parity with Postgres
- Y2: ClickHouse Identity applied even with ssl_mode=Disable (plain HTTP)
- Y3: MySQL VerifyCa == VerifyFull (hostname check not skipped for verify-ca)
- Y4: Lua transform path has no timeout — only command dispatch is guarded

### Medium (4):
- O1: Postgres VerifyCa==VerifyFull design decision undocumented to users
- O2: ClickHouse Certificate::from_pem only parses first cert in bundle (vs rustls full bundle)
- O3: set_timeout_extends_budget uses os.clock() (CPU) vs Instant (wall) — CI flaky risk
- O4: Keyring test doesn't verify D-Bus runtime availability
