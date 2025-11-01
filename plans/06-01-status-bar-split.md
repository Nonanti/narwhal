# Plan 06-01 — Status bar split into three slots

## Why

The current single-line status bar mashes mode, focused pane,
connection, and last message into one string separated by `·`.
Long messages truncate, transient messages overwrite the persistent
connection display, and the user has nowhere to look for "what
connection am I on" at a glance.

Three independent slots fix all of those:

- **Left**:   mode (NOR / INS / CMD) + focused pane label
- **Center**: connection name + driver (sticky, never overwritten)
- **Right**:  last message (transient or pinned by severity)

Optional fourth slot for transaction state when one is open.

## Constraints

- Behaviour-preserving for tests that don't touch the status bar.
- Snapshot tests will need to be re-recorded — that is expected.
- `clippy --all-targets -- -D warnings` clean, `fmt --check` clean.
- AGENTS.md: no `unwrap()`/`expect()` in production code.
- One commit, conventional, long-form.
- NixOS host: every cargo invocation through `nix develop --command`.

## Concrete steps

1. Replace the single `status_message: String` field on `AppCore`
   with a `StatusBar` struct:

   ```rust
   #[derive(Debug, Default, Clone)]
   pub struct StatusBar {
       /// Center slot — set once on connect, cleared on disconnect.
       pub connection: Option<String>,
       /// Right slot — last transient message.
       pub message: String,
       /// Optional fourth slot — open transaction's isolation level.
       pub transaction: Option<String>,
   }
   ```

   `status_message: &str` accessor stays for backward compatibility
   with existing tests but delegates to `self.status.message`.

2. Update every existing `self.status_message = "..."` assignment
   to `self.status.message = "...".into()`. There are ~50 such
   sites; use a single-file find-and-replace then walk the diff.

3. In `core.rs` `execute_command` for `:open` / `:close`:
   - on success: `self.status.connection = Some(format!("{name} · {driver}"));`
   - on close:   `self.status.connection = None;`

4. In `:begin` / `:commit` / `:rollback`:
   - on `:begin`: `self.status.transaction = Some(isolation_label.into());`
   - on `:commit` / `:rollback`: `self.status.transaction = None;`

5. In `narwhal-tui/src/layout.rs`, find where the status bar is
   rendered (single `Paragraph` call). Replace with a three-column
   `Layout::horizontal` split:
   - Left  = `Constraint::Length(left_text.width() + 2)`
   - Right = `Constraint::Min(20)`
   - Center fills the rest.
   - When `transaction.is_some()`, insert a fourth slot before the
     message (yellow text on the theme's `accent` colour).

6. Mode label colours follow the existing accent / muted theme:
   - NOR → muted background
   - INS → accent background
   - CMD → warning background (new theme field; default = ratatui yellow)

7. `Theme` struct in `narwhal-tui` gains a `warning: Color` field
   (default `Color::Yellow`) for the CMD highlight.

## Files

- `crates/narwhal-app/src/core.rs`
  (StatusBar struct, ~50 assignment site updates, connection /
   transaction lifecycle)
- `crates/narwhal-tui/src/lib.rs` or `layout.rs`
  (three-slot status render)
- `crates/narwhal-tui/src/widgets.rs`
  (theme.warning addition if Theme lives here)
- `crates/narwhal-app/tests/snapshots/*.snap`
  (re-record with `INSTA_UPDATE=always cargo test --test snapshots`)
- `crates/narwhal-app/tests/headless.rs`
  (extend existing tests that assert against status_message to
   pull from the new struct fields where appropriate)

## Tests

Two new unit tests:

1. `status_bar_pins_connection_through_transient_messages`: open a
   connection, run a query that prints a status message,
   assert `status.connection` is still set.
2. `status_bar_clears_connection_on_close`: open then close,
   assert `status.connection` is None.

Existing snapshot tests will fail until re-recorded; fold that into
the same commit.

Acceptance: total test count rises by **2**.

## Acceptance

- `nix develop --command cargo fmt --all -- --check` clean.
- `nix develop --command cargo clippy --all-targets -- -D warnings` clean.
- `nix develop --command cargo test --all` reports +2 from baseline.
- Snapshots committed in same commit as the code changes.

## Commit message template

```
feat(tui): split the status bar into mode / connection / message slots

The single-line status bar collapsed mode, focus, connection, and
the last message into one string separated by middle dots. Long
messages truncated visible state, transient messages overwrote
the connection display, and there was no fixed place to look for
"what am I connected to" at a glance.

Replace the AppCore::status_message field with a StatusBar struct
that carries the slots independently:

- left   — mode (NOR/INS/CMD) + focused pane (sticky)
- center — connection name + driver (set by :open, cleared by
           :close; sticky across other messages)
- right  — last transient message (the field every other piece of
           code already writes through)

A fourth optional slot appears when a transaction is open and
shows the isolation level so the user knows their dispatch path.

TUI layout switches to a three-column horizontal split with the
mode block colour-coded by mode. The Theme struct gains a warning
field for the CMD highlight (default ratatui yellow).

Two new unit tests pin the slot independence: connection stays
through transient messages, and connection clears on :close.
Existing TUI snapshots re-recorded with the new layout.
```
