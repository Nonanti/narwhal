# G5 — Plugin/Ext + Network Review

**Scope:** ~12K LOC across narwhal-plugin, narwhal-plugin-lua, narwhal-plugin-wasm, narwhal-lsp, narwhal-audit, narwhal-mcp, narwhal-domain, narwhal binary.  
**Date:** 2026-06-05  
**Status:** clippy ✓ | fmt ✓ | check ✓

---

## 1. narwhal-plugin (660 LOC)

### 1.1 Plugin trait surface — GOOD

| Aspect | Verdict | Notes |
|--------|---------|-------|
| `on_event` default no-op | ✅ | `lib.rs:170-175` — backward-compatible; existing plugins compile without override |
| `PluginEvent` `#[non_exhaustive]` | ✅ | `lib.rs:94` — correct discipline; `translate_event` in `instance.rs:118` has a wildcard arm with `debug_assert!` + `tracing::error` |
| `CommandOutcome` `#[non_exhaustive]` | ✅ | `lib.rs:62` — same pattern |
| `PluginError` `#[non_exhaustive]` | ✅ | `lib.rs:26` — good |

### 1.2 PluginRegistry — GOOD

| Aspect | Verdict | Notes |
|--------|---------|-------|
| `broadcast_event` fan-out | ✅ | `lib.rs:242-253` — iterates all plugins, collects errors, never cancels delivery |
| `TransformErrors` shape | ⚠️ S3 | `lib.rs:286-293` — `pub Vec<String>` field is not `#[non_exhaustive]`. If you later change the inner representation (e.g. add `plugin_name` per error), downstream matches break. Consider making the field private with an accessor. |
| Reserved-builtins guard | ✅ | `lib.rs:203-210` — validates before mutating state; partial registration impossible |
| `register` atomicity | ✅ | `lib.rs:208-230` — validates all commands first, then allocates Arc and inserts |
| `catalogue` allocation | ⚠️ S4 | `lib.rs:268-277` — clones `plugin_name` per command descriptor. Insignificant at current scale but `CommandDescriptor` already has owned `String` fields; the extra clone is avoidable by returning `(plugin_name: &str, &CommandDescriptor)` — low priority. |

### 1.3 Finding

- **[S3] `TransformErrors` is a `pub struct` with a `pub` field** — `lib.rs:286`. Publishing this crate with a public `Vec<String>` field locks the representation. Add `#[non_exhaustive]` and make the field `pub(crate)` with a `iter()` / `messages()` accessor before v2.1.

---

## 2. narwhal-plugin-lua (1.2K LOC)

### 2.1 mlua API binding safety — GOOD

| Aspect | Verdict | Notes |
|--------|---------|-------|
| `std::sync::Mutex` (not tokio) | ✅ | `lib.rs:66-68` — explicitly documented; `blocking_lock()` would panic from async. `spawn_blocking` is the correct bridge. |
| Non-re-entrancy guard | ✅ | `lib.rs:70-77` — doc clearly states deadlock risk; `sql_run` goes to pool, not back into the same plugin. |
| LuaSandbox::Restricted | ✅ | `lib.rs:83-97` — explicit `StdLib` bit-or so new mlua versions don't silently load extra libs. |
| `load` in restricted mode | ✅ | `lib.rs:428-439` — `load` is available but inherits restricted globals, so `os.execute` still fails. Test confirms. |

### 2.2 AppPluginExecutor / connection state — GOOD

| Aspect | Verdict | Notes |
|--------|---------|-------|
| `install_executor` timing | ✅ | `lib.rs:226` — "Calling this twice replaces the previously-installed executor." Documented. |
| `block_in_place` + `Handle::block_on` | ✅ | `lib.rs:242-244` — inside `spawn_blocking` already; safe per Tokio docs. |

### 2.3 Timeout budget — GOOD

| Aspect | Verdict | Notes |
|--------|---------|-------|
| Registry-stored budget | ✅ | `lib.rs:276-287` — `narwhal_timeout_budget` in Lua registry, not on `narwhal` global. Scripts can't tamper. Test `script_cannot_clear_timeout_budget` confirms. |
| NaN / negative / huge values | ✅ | `lib.rs:279-288` — `Duration::MAX` (disabled) for non-finite / negative / overflow. Tests confirm. |
| `sql_run` respects budget | ✅ | `lib.rs:248-260` — `tokio::time::timeout` wraps the executor call when budget is finite. |
| EVERY_LINE hook | ✅ | `lib.rs:311-327` — installed and removed per invocation; no stale hook. |

### 2.4 Auto-load directory — MINOR CONCERN

| Aspect | Verdict | Notes |
|--------|---------|-------|
| Path validation | ⚠️ S3 | `from_path` (`lib.rs:288-299`) reads any file the process can access. No symlink/canonicalization guard — a symlink in the plugins dir pointing at `/etc/shadow` would be read as Lua source (and fail at parse, leaking nothing). The threat is minimal (parse failure, not execution), but documenting the trust boundary is worthwhile. |
| Error mode on load failure | ⚠️ S4 | The auto-load loop in `App` presumably logs and skips. `from_path` returns `PluginError::Runtime` which the host could treat as fatal. No issue in practice — just noting the host must handle it gracefully. |

### 2.5 Findings

- **[S3] `from_path` no symlink/canonicalization check** — `lib.rs:288-299`. A symlink in the plugins directory could point to an arbitrary file. The Lua VM will fail to parse it as Lua, but the file contents *are* read into memory. Consider documenting the trust assumption ("plugins directory must be operator-controlled") or adding an optional `resolve_symlinks` guard.
- No other issues. The Lua runtime is well-structured and defensively coded.

---

## 3. narwhal-plugin-wasm (4K LOC) — MOST CRITICAL

### 3.1 Engine/Linker shared safety — GOOD

| Aspect | Verdict | Notes |
|--------|---------|-------|
| Single `Engine` per `Runtime` | ✅ | `runtime.rs:138-147` — built once in `Runtime::build`, shared by all plugins. |
| `Linker<HostState>` in Arc | ✅ | `runtime.rs:88` — `Arc<Linker<HostState>>` cloned per `Runtime::clone`, same underlying linker. |
| `add_to_linker` with `HostMarker` | ✅ | `host.rs:30-35` — correct `HasData` impl; `Data = &'a mut HostState`. |
| Bindgen `imports: { default: async \| trappable }` | ✅ | `lib.rs:47-53` — host fns return `wasmtime::Result<T>`, so capability denial produces a trap (not a silent drop). |

### 3.2 HostState scoping — GOOD

| Aspect | Verdict | Notes |
|--------|---------|-------|
| Per-plugin KV namespace | ✅ | `host.rs:85` — `kv: AsyncMutex<HashMap<String, Vec<u8>>>`. The namespace is implicit (each plugin has its own `HostState`); one plugin cannot see another's keys. |
| Per-plugin capability set | ✅ | `host.rs:84` — `enforcer: Arc<dyn Enforcer>` built from the manifest's intersected set at load time. |
| Per-plugin fuel budget | ✅ | `instance.rs:65` — `refuel` called before every export with `runtime.config().fuel_per_call`. |

### 3.3 Capability check architecture — GOOD (with one concern)

| Aspect | Verdict | Notes |
|--------|---------|-------|
| Manifest declare → host grants validate → load refused | ✅ | `runtime.rs:164-173` — coarse settings gate, then fine grants gate. Both must pass before `Component::from_file`. |
| Per-call enforcement | ✅ | `host.rs:108-157` — every host fn entry routes through `enforcer.check`. Denial → trap (cmd/state-set) or `None` (state-get). |
| Hard-trap for cmd/state-set | ✅ | `host.rs:112-124`, `host.rs:139-147` — returns `wasmtime::Error::msg(...)`. |
| `None` return for state-get | ✅ | `host.rs:127-132` — prevents cardinality probing. |
| FsRead/FsWrite/Net/Env per-call enforcement | ⚠️ S2 | These are enforced at *manifest load time* only. The `Operation::FsRead`/`FsWrite`/`NetConnect`/`EnvRead` variants exist in the `Enforcer` but are never constructed from host fns — because the WIT surface doesn't expose `host.fs-read` etc. yet. The enforcer is *ready* but *not wired*. This is explicitly documented as deferred (T1-T5-B). **Not a bug, but the public surface publishes `Operation` variants that can never be hit in v2.0.** |

### 3.4 Fuel, memory, KV enforcement — GOOD

| Aspect | Verdict | Notes |
|--------|---------|-------|
| `DEFAULT_MEMORY_LIMIT` = 64 MiB | ✅ | `runtime.rs:26` — enforced via `StoreLimitsBuilder`. |
| `DEFAULT_FUEL_BUDGET` = 100M | ✅ | `runtime.rs:30` — `set_fuel` before each export; `FuelMeter` tracks consumption. |
| `DEFAULT_KV_BUDGET` = 256 KiB | ✅ | `runtime.rs:33` — `KvAccount::project` + `commit` pattern; overruns trap. |
| Lock ordering in `state_set` | ✅ | `host.rs:141-143` — `kv` lock first, then `kv_account`. Documented as "kv → kv_account, never reversed". |

### 3.5 Per-Store mutex contention — MINOR CONCERN

| Aspect | Verdict | Notes |
|--------|---------|-------|
| `tokio::sync::Mutex` serializes calls | ⚠️ S3 | `instance.rs:56-58` — every `dispatch`/`deliver_event` locks the store mutex. Wasmtime stores are not `Sync`, so this is *necessary*, not avoidable. But: for a plugin that handles many events (e.g. `EditorBufferChanged` on every keystroke), every call is serialized. This is acceptable for v2.0 but should be documented as a potential bottleneck. |

### 3.6 Manifest TOML schema — GOOD

| Aspect | Verdict | Notes |
|--------|---------|-------|
| Required fields | ✅ | `name`, `version`, `api-version` are required via `RawManifest` deserialization. |
| `api-version` check | ✅ | `manifest.rs:86-92` — `HOST_API_MAJOR = 1`; mismatch returns `WasmError::ApiVersion`. |
| Component path default | ✅ | `manifest.rs:112-115` — defaults to `<name>.wasm` relative to manifest. |
| Capability token parsing | ✅ | `parser.rs` — legacy unit forms + explicit `kind.action:arg` forms. Path traversal rejected. |
| `#[serde(deny_unknown_fields)]` on RawManifest | ✅ | Future manifest additions require a code change, which is the correct conservative posture. |

### 3.7 Public surface stability — CONCERN

| Aspect | Verdict | Notes |
|--------|---------|-------|
| `Operation` enum is `#[non_exhaustive]` | ✅ | `sandbox/operation.rs:13` — adding variants is non-breaking. |
| `Capability` enum is `#[non_exhaustive]` | ✅ | `capability/mod.rs:32` — same. |
| `CapabilityKind` is `#[non_exhaustive]` | ✅ | `capability/set.rs:26` — same. |
| `Decision` is `#[non_exhaustive]` | ✅ | `sandbox/decision.rs:34` — same. |
| `WasmError` is `#[non_exhaustive]` | ✅ | `error.rs:11` — same. |
| `RuntimeConfig` is `#[non_exhaustive]` | ✅ | `runtime.rs:48` — same. |
| `bindings` module re-export | ⚠️ S3 | `lib.rs:60-71` — explicitly documented as "implementation-defined, semver-fragile". Good. But the `#[allow(dead_code, unreachable_pub, ...)]` suppressions on the `bindgen!` output mean that if bindgen generates a new public type that breaks a downstream consumer, they won't see the warning. This is intentional and acceptable. |
| `StandardEnforcer::new` takes `broad_cmd` as a separate arg | ⚠️ S4 | `sandbox/enforcer.rs:32-33` — this duplicates `Grants::broad_cmd`. If they diverge, subtle bugs. Currently the runtime passes `self.config.grants.broad_cmd` in `standard_enforcer()`, so they're consistent. Low risk. |

### 3.8 Findings

- **[S2] FsRead/FsWrite/NetConnect/EnvRead operations are enforcer-ready but unwired** — The `Operation` enum and `Enforcer::match_fs/match_net/match_env` methods exist in the public surface. Downstream code that calls `enforcer.check("p", &Operation::FsRead{...})` works fine, but no host fn ever constructs these. This is correct for v2.0 (deferred to T1-T5-B). The risk: if a consumer builds an `Operation::FsRead` and calls `check`, it will correctly evaluate against the effective set — but the *audit log* will show a denial that never originated from a real WASM trap. This is a semantic correctness issue for audit consumers, not a security issue. Document the "not yet wired" status on each variant.
- **[S3] Per-Store mutex serialization** — `instance.rs:56`. Acceptable for v2.0 but should be noted as a scalability ceiling. If a future plugin needs concurrent event handling, the architecture would need per-export locking or an event queue.

---

## 4. narwhal-lsp (1.2K LOC) — ORPHAN CRATE

### 4.1 Protocol primitives — GOOD

| Aspect | Verdict | Notes |
|--------|---------|-------|
| JSON-RPC 2.0 framing | ✅ | `transport.rs:68-93` — Content-Length read, `\r\n\r\n` delimiter, body read. Unknown headers ignored. |
| `initialize`/`initialized` | ✅ | `client.rs:236-242` — typed wrappers over `request`/`notify`. |
| `textDocument/completion`, `hover`, `definition` | ✅ | `client.rs:250-259` — typed params, typed returns. |
| `shutdown`/`exit` | ✅ | `client.rs:268-275` — sends both, then awaits join. |
| Server notifications | ✅ | `client.rs:262-265` — `next_notification()` reads from bounded mpsc. |

### 4.2 Transport — GOOD

| Aspect | Verdict | Notes |
|--------|---------|-------|
| Stdio transport | ✅ | `transport.rs:54-68` — `BufReader<ChildStdout>` / `ChildStdin`. `write_framed` flushes. |
| MemoryTransport | ✅ | `transport.rs:113-207` — waker-based polling; `close()` resolves pending recv. |
| Read-frame EOF | ✅ | `transport.rs:91` — `n == 0` → `Ok(None)`. |

### 4.3 Bounded notification queue (M4 fix) — GOOD

| Aspect | Verdict | Notes |
|--------|---------|-------|
| `NOTIFICATION_QUEUE_CAPACITY` = 256 | ✅ | `client.rs:120` — `mpsc::channel` (bounded). |
| `try_send` on full → drop + count | ✅ | `client.rs:307-319` — `try_send`, `TrySendError::Full` increments `notifications_dropped`. |
| Counter exposed | ✅ | `client.rs:191-196` — `notifications_dropped()` with `Relaxed` ordering. |

### 4.4 Per-request timeout (M5 fix) — GOOD

| Aspect | Verdict | Notes |
|--------|---------|-------|
| `DEFAULT_REQUEST_TIMEOUT` = 10s | ✅ | `client.rs:124` — applied in `request`. |
| `request_with_timeout` | ✅ | `client.rs:137-175` — `tokio::time::timeout`, sends `Outbound::Cancel(id)` on expiry. |
| Cancel path (MR-C2) | ✅ | `client.rs:160-165` — `id_slot` publishes the id before await so cancel can find it. |
| Residual leak guard | ✅ | `client.rs:217-221` — `if responder.is_closed() { continue; }` skips stale requests. |

### 4.5 Timeout method name (MR-N9) — GOOD

| Aspect | Verdict | Notes |
|--------|---------|-------|
| `LspError::Timeout(String)` carries method name | ✅ | `client.rs:86-87` — "LSP request 'completion' timed out" vs generic message. |

### 4.6 Editor wiring deferred — CONCERN

| Aspect | Verdict | Notes |
|--------|---------|-------|
| Public surface completeness | ⚠️ S2 | The crate exposes `ClientHandle::completion`, `hover`, `did_open`, `did_change` — the full lifecycle. But `AppCore` doesn't wire these yet. The API is *ready* but *untested under real editor integration*. The `CompletionPopupView` in `narwhal-domain` references `CompletionItemView` which the LSP would populate. The types match, but there's no integration test. **Breaking risk**: if the LSP server returns an unexpected shape (e.g. `CompletionResponse::List` vs `CompletionResponse::Array`), the client's typed deserialization will fail at runtime. |
| `ClientHandle` is `Clone` but not `Debug` | ⚠️ S4 | `ClientHandle` derives `Clone` but not `Debug`. The `mpsc::Sender<Outbound>` and `Arc<Mutex<mpsc::Receiver<...>>>` don't impl Debug. Not a bug, but diagnostic logging of the handle would help debugging. |

### 4.7 Findings

- **[S2] LSP client is untested under real editor integration** — The `CompletionResponse` type from `lsp-types` has two variants (`Array` and `List`). The client's `request` method deserializes generically via serde; if the server returns the `List` variant, it works. But the `completion()` method returns `Option<CompletionResponse>` which covers both. **However**, there's no integration test that verifies `sqls` or `sqlls` actually speaks the expected protocol shape. The crate is correct by construction but the public surface hasn't been stress-tested. Before v2.1 wiring, add at least one integration test against a real sqls binary (or a mock that returns both `CompletionResponse` variants).
- **[S4] `ClientHandle` missing `Debug`** — `client.rs:128`. Add `#[derive(Debug)]` or implement manually for diagnostics.

---

## 5. narwhal-audit (1.6K LOC)

### 5.1 Service shutdown — GOOD

| Aspect | Verdict | Notes |
|--------|---------|-------|
| `Notify` pattern (not Sender::closed) | ✅ | `service.rs:88` — `shutdown: Arc<Notify>`. Worker selects on both `rx.recv()` and `shutdown.notified()`. |
| `rx.close()` on shutdown | ✅ | `service.rs:193-197` — closes receiver, waking any `block_on_full` emitters with `SendError::Closed`. No deadlock. |
| Test confirms | ✅ | `service.rs:262-285` — `block_on_full_shutdown_does_not_deadlock` test. |
| Idempotent shutdown | ✅ | `service.rs:167-175` — `join.take()` inside a tight scope; second call returns immediately. |

### 5.2 File sink rotation — GOOD

| Aspect | Verdict | Notes |
|--------|---------|-------|
| Drop-handle (M2 fix) | ✅ | `sinks/file.rs:96-100` — `state.file.take()` + `sync_data` + `drop`. No `/dev/null` placeholder. |
| Collision-safe stamps (M3 fix) | ✅ | `sinks/file.rs:106-138` — `pick_rotated_name_atomic` uses `create_new` (atomic claim), then `rename`. Placeholder cleaned up on rename failure. |
| Suffix format MR-N4 | ✅ | `sinks/file.rs:108` — `%Y%m%dT%H%M%S%3fZ` (ms resolution). Numeric `-N` suffix for same-ms collision. Nanosecond fallback as last resort. `MAX_ROTATION_ATTEMPTS` = 1024. |
| Block-mode shutdown safety | ✅ | Worker drains in-flight events before flushing sinks. |

### 5.3 Redactor — GOOD

| Aspect | Verdict | Notes |
|--------|---------|-------|
| SQL secret reuse from `narwhal_history::redact_sql_secrets` | ✅ | `redactor.rs:6-9` — documented as best-effort, not a security boundary. |
| ASCII case folding consistency | ✅ | `redactor.rs:36-42` — rules folded with `to_ascii_lowercase()`, matched with `eq_ignore_ascii_case()`. Test confirms. |
| Column-name redaction | ✅ | `redactor.rs:69-77` — `<column>=***` format; matches column name against rules. |

### 5.4 Back-pressure — GOOD

| Aspect | Verdict | Notes |
|--------|---------|-------|
| Lossy mode (default) | ✅ | `service.rs:138-148` — `try_send` + `TrySendError::Full` → `tracing::warn`. |
| Block mode | ✅ | `service.rs:133-136` — `send().await` blocks emitter. |
| Channel capacity = 1024 | ✅ | `service.rs:21` — configurable via builder. |

### 5.5 File ownership — MINOR CONCERN

| Aspect | Verdict | Notes |
|--------|---------|-------|
| No umask/permissions setting | ⚠️ S3 | `sinks/file.rs:82-85` — `create_dir_all` + `create+append`. The file inherits the process's umask. For compliance deployments, an explicit `chmod 0640` (or `mode` on `OpenOptions`) after creation would ensure audit logs aren't world-readable. Not a bug — the umask is typically 022 — but worth documenting or adding a `file_permissions` config knob. |

### 5.6 Audit emit sites — GOOD

| Aspect | Verdict | Notes |
|--------|---------|-------|
| ConnectionOpened (sync + spawn) | ✅ | Per memory note; spawn avoids blocking the connection setup. |
| ConnectionClosed (async) | ✅ | Awaits the close_session. |
| PluginLoaded (register order BEFORE plugins-dir) | ✅ | Per memory note; audit service installed before plugin dir walk. |
| Configuration (added/updated/removed/forget_password) | ✅ | Multiple config-change events covered. |

### 5.7 Findings

- **[S3] No explicit file permissions on audit log** — `sinks/file.rs:82-85`. Consider adding a `file_mode` option to `FileSinkConfig` (default `0o640`) and applying it after open via `std::fs::set_permissions` on Unix. This is a compliance concern, not a correctness bug.

---

## 6. narwhal-mcp (3.3K LOC)

### 6.1 Handshake protocol version — GOOD

| Aspect | Verdict | Notes |
|--------|---------|-------|
| `MCP_PROTOCOL_VERSION` = "2024-11-05" | ✅ | `protocol.rs:7` — matches the first stable MCP spec. |
| `validate_jsonrpc` | ✅ | `protocol.rs:34-39` — checks `jsonrpc == "2.0"` before dispatch. |
| `Request._jsonrpc` field | ⚠️ S4 | `protocol.rs:14` — prefixed with `_` (unused after validation). This is fine — the field is read by `validate_jsonrpc` — but the underscore prefix suggests it's intentionally unused, which is slightly misleading. Not worth changing. |

### 6.2 Tool registration collision policy — GOOD

| Aspect | Verdict | Notes |
|--------|---------|-------|
| `RegistrationOutcome::CollisionBuiltin` | ✅ | `tools/mod.rs:177` — built-ins always win. |
| `RegistrationOutcome::CollisionDynamic` | ✅ | `tools/mod.rs:179-181` — first dynamic registration wins; carries `existing_source`. |
| Tests confirm | ✅ | `tools/mod.rs` tests cover both collision paths. |

### 6.3 Dynamic tool descriptors — GOOD

| Aspect | Verdict | Notes |
|--------|---------|-------|
| No `Box::leak` (C1 fix) | ✅ | `DynamicTool` owns its `String` fields. `ToolRegistry::dynamic` stores `Arc<DynamicTool>`. |
| `Cow<'static, str>` (MR-N3) | ✅ | `protocol.rs:112-113` — `ToolDescriptor::name` and `description` are `Cow<'static, str>`. Built-ins use `Cow::Borrowed`. |
| `descriptor_name()`/`descriptor_description()` | ✅ | `tools/mod.rs:87-96` — built-ins override with `Cow::Borrowed`; default impl uses `Cow::Owned`. |

### 6.4 Response size cap (C2/C3) — GOOD

| Aspect | Verdict | Notes |
|--------|---------|-------|
| `MAX_RESPONSE_BYTES` = 512 KiB | ✅ | `tools/mod.rs:40` — enforced centrally in `McpServer::handle_tools_call`. |
| `cap_response` | ✅ | `tools/mod.rs:48-82` — JSON envelope with `truncated: true`, snippet, and reason. |
| Per-cell cap (`MAX_CELL_BYTES` = 64 KiB) | ✅ | `tools/run_query.rs:40` — truncates individual cells. |
| Central cap applies to all tools | ✅ | `server.rs:130-134` — `cap_responses(output.text, &params.name)` in `handle_tools_call`. |

### 6.5 run_query read_only enforcement — GOOD

| Aspect | Verdict | Notes |
|--------|---------|-------|
| Statement guard | ✅ | `run_query.rs:122-130` — `guard_read_only` rejects non-SELECT/WITH/SHOW/EXPLAIN/DESCRIBE/PRAGMA/VALUES. |
| BEGIN/ROLLBACK sandwich | ✅ | `run_query.rs:203-214` — unconditional rollback. BEGIN failure falls through to bare execute (for drivers without transactions). |
| `--read-only` CLI flag | ✅ | `context.rs:88-95` — `force_read_only` trumps workspace ACL. |
| Workspace ACL | ✅ | `context.rs:84-86` — `writes_allowed()` checks workspace + force flag. |

### 6.6 workspace.toml integration — GOOD

| Aspect | Verdict | Notes |
|--------|---------|-------|
| Discovery | ✅ | `workspace.rs:66-85` — walks up from `cwd`, finds `.narwhal/workspace.toml`. |
| `allowed_connections` | ✅ | `workspace.rs:97-102` — empty list = allow all. |
| `allow_writes` | ✅ | `workspace.rs:46-48` — defaults to `true`. |
| `logical_relations` | ✅ | `workspace.rs:50-54` — passed to `get_diagram`. |
| `deny_unknown_fields` | ✅ | `workspace.rs:38` — typos surface as parse errors. |

### 6.7 get_diagram MermaidRenderer integration — GOOD

| Aspect | Verdict | Notes |
|--------|---------|-------|
| `MermaidRenderer`/`DotRenderer` | ✅ | `get_diagram.rs:7-8` — from `narwhal_diagram`. |
| Logical relations from workspace | ✅ | `get_diagram.rs:139-146` — `collect_logical_relations_for`. |
| Focused diagram (1-hop) | ✅ | `get_diagram.rs:148-151` — `diagram_focused`. |

### 6.8 Findings

- **[S3] `explain_query` and `run_query` have duplicated `run_in_sandbox`** — `run_query.rs:203-214` and `explain_query.rs:156-166` contain nearly identical `run_in_sandbox` functions. The comment in `explain_query.rs:163-165` acknowledges this: "If we add a third user we'll extract it into a small helper module." This is a DRY violation but functionally correct. Extract before v2.1.
- **[S4] `Request._jsonrpc` field naming** — `protocol.rs:14`. The `_` prefix is misleading since the field is read. Very low priority.

---

## 7. Binary (narwhal/src/main.rs)

### 7.1 Mode parsing — GOOD

| Aspect | Verdict | Notes |
|--------|---------|-------|
| `exec`, `mcp`, `audit tail`, `schema-diff`, `migrate-config` | ✅ | All subcommands properly typed. |
| `--read-only` global flag | ✅ | `main.rs:44-45` — propagated to every mode. |
| No args = TUI | ✅ | `main.rs:69` — `None => run_tui(...)`. |

### 7.2 App builder order — GOOD

| Aspect | Verdict | Notes |
|--------|---------|-------|
| `with_audit_service` BEFORE `with_plugins_dir` | ✅ | `main.rs:274-278` — audit installed before plugins load, so `PluginLoaded` events are captured. |

### 7.3 resolve_audit_path — GOOD

| Aspect | Verdict | Notes |
|--------|---------|-------|
| strftime template | ✅ | `main.rs:395-410` — `chrono::Utc::now().format(&template)`. |
| First `file:` sink from settings | ✅ | `main.rs:400-409` — iterates `settings.audit.sinks`, finds first `SinkSpec::File`. |

### 7.4 Error mode / exit codes — GOOD

| Aspect | Verdict | Notes |
|--------|---------|-------|
| `anyhow` at binary level | ✅ | All `?` operators propagate anyhow errors. |
| `--fail-on-drift` → exit code 2 | ✅ | `main.rs:476-483` — `std::process::exit(2)`. |
| Config validation → exit code 1 | ✅ | `main.rs:194` — `std::process::exit(1)`. |

### 7.5 Findings

- No significant issues. The binary is well-structured with clean separation between modes.

---

## 8. narwhal-domain (1.3K LOC)

### 8.1 Shape / purpose — GOOD

| Aspect | Verdict | Notes |
|--------|---------|-------|
| `EditorBuffer` | ✅ | `editor.rs` — comprehensive text buffer with cursor, multi-cursor, auto-pair, word motions. |
| `Motion` | ✅ | `motion.rs` — domain-level cursor motion enum, independent of vim crate. |
| `SchemaListing` | ✅ | `schema.rs` — type alias for `(Schema, Vec<Table>)`. |
| `#![forbid(unsafe_code)]` | ✅ | `lib.rs:6` — enforced. |

### 8.2 Dead code / naming consistency — GOOD

| Aspect | Verdict | Notes |
|--------|---------|-------|
| No dead code | ✅ | All three modules are `pub` and re-exported. `EditorSearchHighlight` has a `'a` lifetime and is used by the TUI renderer. |
| Naming consistency | ✅ | `EditorBuffer`, `Motion`, `SchemaListing` are clear domain names. |

### 8.3 Findings

- **[S4] `SchemaListing` is a type alias, not a struct** — `schema.rs:6`. Type aliases don't show up in docs well and can't have `Debug`/`Display` impls. Consider making it a newtype if it acquires methods. Current usage is fine.
- **[S4] `floor_char_boundary` is public** — `editor.rs:712-718`. This helper is exposed as `pub fn` but is only used internally by the editor. It duplicates the eventually-stable `str::floor_char_boundary`. Mark it `pub(crate)` or feature-gate it.

---

## Summary of Findings by Severity

### S2 — Should fix before v2.1 (breaking risk if published)

| # | Crate | Location | Issue |
|---|-------|----------|-------|
| 1 | narwhal-lsp | client.rs (entire crate) | Editor wiring deferred — public surface untested under real sqls/sqlls integration. `CompletionResponse` variants, notification routing, and cancellation all need integration validation before v2.1 commit. |
| 2 | narwhal-plugin-wasm | sandbox/operation.rs:17-30 | `FsRead`/`FsWrite`/`NetConnect`/`EnvRead` `Operation` variants are in public surface but never wired to host fns. Audit consumers may see "denied" events that never originated from a real WASM trap. Document "deferred" status on each variant. |

### S3 — Worth fixing before v2.1 (representation locked or minor security gap)

| # | Crate | Location | Issue |
|---|-------|----------|-------|
| 3 | narwhal-plugin | lib.rs:286 | `TransformErrors` has a `pub Vec<String>` field. If the internal representation changes, downstream breaks. Add `#[non_exhaustive]` + private field with accessor. |
| 4 | narwhal-plugin-lua | lib.rs:288-299 | `from_path` reads arbitrary file via `read_to_string` without symlink canonicalization. Document trust assumption for plugins directory. |
| 5 | narwhal-plugin-wasm | instance.rs:56-58 | Per-Store `tokio::sync::Mutex` serializes all WASM calls for one plugin. Noted as scalability ceiling for high-frequency event plugins. |
| 6 | narwhal-audit | sinks/file.rs:82-85 | No explicit file permissions on audit log files. Default umask may leave files world-readable. Add `file_mode` config option. |

### S4 — Low priority (cosmetic / DRY / minor)

| # | Crate | Location | Issue |
|---|-------|----------|-------|
| 7 | narwhal-plugin | lib.rs:268-277 | `catalogue()` clones `plugin_name` per descriptor. Avoidable with borrowed return. |
| 8 | narwhal-lsp | client.rs:128 | `ClientHandle` missing `Debug` impl. |
| 9 | narwhal-mcp | run_query.rs + explain_query.rs | Duplicated `run_in_sandbox` function. Extract to shared helper. |
| 10 | narwhal-mcp | protocol.rs:14 | `Request._jsonrpc` field `_` prefix misleading (field is used by `validate_jsonrpc`). |
| 11 | narwhal-domain | schema.rs:6 | `SchemaListing` is a bare type alias; consider newtype if it acquires behavior. |
| 12 | narwhal-domain | editor.rs:712 | `floor_char_boundary` is `pub` but only used internally. Mark `pub(crate)`. |
| 13 | narwhal-plugin-lua | lib.rs:288-299 | Auto-load `from_path` error mode: host must handle `PluginError::Runtime` gracefully (skip, don't panic). |
| 14 | narwhal-plugin-wasm | sandbox/enforcer.rs:32-33 | `StandardEnforcer::new` takes `broad_cmd` separately from `Grants`. Minor duplication; currently consistent. |

### Positive observations (no action needed)

- **Plugin trait design**: `#[non_exhaustive]` enums, default no-ops, atomic registration, reserved-builtins guard — all excellent.
- **WASM sandbox architecture**: Engine/Linker sharing, per-plugin `HostState`, fuel/memory/KV limits, decision cache, audit trail — comprehensive and well-documented.
- **WIT contract**: Append-only policy for v2.x, `api-version` check, `bindgen!` with `async | trappable` — correct.
- **Audit service**: `Notify`-based shutdown (no `Sender::closed` deadlock), atomic rotation, strftime paths, lossy/block modes — production-ready.
- **MCP server**: Bounded frames, collision-safe tool registration, centralized response cap, read-only enforcement with belt-and-suspenders, workspace ACL — all solid.
- **Lua sandbox**: `Restricted` mode blocks `io`/`os`/`package`/`debug`, registry-stored timeout budget (tamper-proof), `spawn_blocking` for VM calls — well-designed.
- **Domain crate**: Clean separation, `#![forbid(unsafe_code)]`, comprehensive multi-cursor support, Unicode-aware word motions.
