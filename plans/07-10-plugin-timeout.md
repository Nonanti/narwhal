# Plan 07-10 — Plugin Lua execution timeout

## Why

A plugin with `while true do end` locks the entire TUI forever —
there's no interrupt mechanism and the user has to kill the
process. Plugin authors are also users, so this is reachable in
normal development, and a malicious or buggy third-party plugin
can DoS the editor.

## Scope

- `mlua` exposes an `interrupt` callback that fires periodically
  during Lua execution. Hook it. When the elapsed plugin
  execution time exceeds the configured budget, raise a Lua
  error that bubbles back to the host as a captured failure.
- Default budget: **5 seconds**.
- Plugins that legitimately need longer (a streaming explain, a
  large export) can opt out with a new global
  `narwhal.set_timeout(seconds)` (0 = no timeout).
- A timed-out plugin surfaces in the status bar as
  `plugin <name>: timed out after 5s`.

## Constraints

- AGENTS.md: no `unwrap()` / `expect()` in production code.
- `nix develop --command cargo fmt --all -- --check` clean.
- `nix develop --command cargo clippy --all-targets -- -D warnings` clean.
- One conventional commit, long-form.
- Don't add a thread to wake up Lua — `mlua`'s interrupt is
  call-count-based; we check `Instant::elapsed()` inside the
  callback.
- Per-plugin state (start time, budget) lives in the registered
  Lua context for that invocation, not in a shared global.

## Concrete steps

### Step 1: timeout state

`crates/narwhal-plugin-lua/src/lib.rs`:

```rust
struct InvocationTimeout {
    started_at: Instant,
    budget: Duration,
}

impl InvocationTimeout {
    fn new(budget: Duration) -> Self {
        Self { started_at: Instant::now(), budget: budget.max(Duration::from_millis(1)) }
    }
    fn elapsed(&self) -> Duration { self.started_at.elapsed() }
    fn exceeded(&self) -> bool { self.elapsed() >= self.budget }
}
```

### Step 2: per-invocation wrapper

The host already calls plugin entry points via something like
`lua.call_function::<_, ()>(name, args)`. Wrap each invocation:

```rust
pub async fn invoke_with_timeout(
    &mut self,
    name: &str,
    args: impl IntoLuaMulti,
    budget: Duration,
) -> Result<()> {
    let timeout = Arc::new(Mutex::new(InvocationTimeout::new(budget)));
    let timeout_for_hook = timeout.clone();

    self.lua.set_interrupt(move |_| {
        let t = timeout_for_hook.lock().expect("interrupt mutex");
        if t.exceeded() {
            Err(mlua::Error::RuntimeError(format!(
                "plugin timed out after {:.1}s",
                t.elapsed().as_secs_f64()
            )))
        } else {
            Ok(mlua::VmState::Continue)
        }
    })?;

    let result = self.lua.call_function(name, args);
    // Always clear the interrupt afterwards so the next call
    // doesn't inherit this one's budget.
    self.lua.remove_interrupt();
    result
}
```

(API names depend on the mlua version in use; consult cargo
metadata before wiring.)

### Step 3: `narwhal.set_timeout`

When constructing the Lua context, expose:

```rust
let set_timeout = lua.create_function(|_, secs: f64| {
    // Stash the requested budget on a registry key the next
    // invoke_with_timeout call reads.
    Ok(())
})?;
narwhal_table.set("set_timeout", set_timeout)?;
```

The set value lives on the Lua registry, retrieved at the next
invocation start and used as the budget instead of the default.

### Step 4: status bar on timeout

`AppCore` already routes plugin errors to the status bar (see
the existing `dispatch_plugin_command` error branch). When the
error message starts with `plugin timed out`, format it as
`plugin <name>: timed out after Xs`.

### Step 5: tests

`tests/plugin_timeout.rs`:

1. `infinite_loop_times_out` — register a plugin `while true do
   end`, invoke, assert error after ≥5s (use a 100ms test budget
   instead so the test runs fast).
2. `set_timeout_extends_budget` — plugin calls
   `narwhal.set_timeout(0.5)`, then runs a 200ms busy loop,
   asserts success.
3. `normal_plugin_unaffected` — plugin that returns immediately,
   no timeout error.

Acceptance: +3 tests.

## Files

- `crates/narwhal-plugin-lua/src/lib.rs` (interrupt hook,
  invoke_with_timeout, narwhal.set_timeout)
- `crates/narwhal-plugin-lua/tests/timeout.rs` (new)
- `crates/narwhal-app/src/core.rs` (plugin invocation path uses
  invoke_with_timeout; status message formatting on timeout)

## Acceptance

- `nix develop --command cargo fmt --all -- --check` clean
- `nix develop --command cargo clippy --all-targets -- -D warnings` clean
- `nix develop --command cargo test --all` reports +3 from baseline
- Manual smoke: drop a `while true do end` plugin into
  `~/.config/narwhal/plugins/`, trigger it, watch the status bar
  show the timeout message after 5s.

## Commit message template

```
feat(plugin): Lua execution timeout via mlua interrupt hook

A plugin with `while true do end` locked the entire TUI forever
— there was no interrupt mechanism and the user had to kill the
process.  Plugin authors are also users, so this was reachable
in normal development.

Hook mlua's interrupt callback per invocation: on every check the
hook compares Instant::elapsed() against the configured budget
and, when exceeded, raises a RuntimeError that bubbles back to
the host as a captured failure.  No extra thread; the interrupt
is call-count-based and the elapsed-time check is cheap.

Default budget is 5 seconds — long enough for any reasonable
result-pane transformation, short enough to be obvious when
something went wrong.  Plugins that legitimately need longer
(streaming explain, large export) opt out with
narwhal.set_timeout(seconds), where 0 disables the timeout
entirely.

A timed-out plugin surfaces as `plugin <name>: timed out after
5.0s` in the status bar; the rest of the TUI keeps responding
since the Lua side terminates cleanly.

Three new tests cover the infinite-loop trip, the set_timeout
opt-out path, and the normal-plugin no-op path.  The infinite-
loop test uses a 100ms budget so CI doesn't wait 5 seconds.
```
