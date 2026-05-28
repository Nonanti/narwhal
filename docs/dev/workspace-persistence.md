# Workspace persistence

Design notes for the workspace-state restore path (open tabs, cursor
position, sidebar expansion) introduced in v2.0. A forward-compat
slot `PersistedSidebar::expanded_schemas` is reserved for upcoming
collapsible-sidebar support.

## Headline

`narwhal_app` gains a `persist` module that snapshots the editor's
tab list, cursor / scroll positions, sidebar viewport state and
active-connection name on **clean exit**, then replays them on the
next launch. The snapshot lives at
`~/.config/narwhal/workspace-state.toml` — TOML, atomic-rename
writes, opt-out via `[settings.workspace.persist]`.

The whole feature defaults **on**. A fresh install gets DataGrip /
VS-Code style "reopen where I left off" behaviour without any
config-file editing; the only switch users need to flip is
`enabled = false` for the v1 stateless start-up.

## Public surface delta

```text
crates/narwhal-app:
+ pub mod persist;
+ pub use persist::{
+  CURRENT_SCHEMA_VERSION, PersistError, PersistResult,
+  PersistedSidebar, PersistedTab, PersistedTabKind,
+  PersistedWorkspace, SaveOutcome,
+  load_at_start, save_at_exit, snapshot, apply,
+ };

crates/narwhal-config:
+ ConfigPaths::workspace_state_file  // ~/.config/narwhal/workspace-state.toml
~ impl Default for WorkspacePersistSettings // every knob defaults to true (v1 had no opinion)
```

Concrete shapes (all `#[non_exhaustive]` per
`docs/dev/api-surface.md`):

```rust
pub struct PersistedWorkspace {
  pub schema_version: u32,  // 1 for v2.0
  pub narwhal_version: Option<String>,
  pub saved_at: Option<String>,
  pub active_connection: Option<String>,
  pub tabs: Vec<PersistedTab>,
  pub active_tab: usize,
  pub sidebar: PersistedSidebar,
}
impl PersistedWorkspace {
  pub fn empty -> Self;
  pub fn with(f: impl FnOnce(&mut Self)) -> Self;
}

pub struct PersistedTab {
  pub name: String,
  pub buffer: String,
  pub cursor_row: usize,
  pub cursor_col: usize,
  pub scroll: usize,
  pub kind: PersistedTabKind,  // SqlEditor (only variant in v2.0)
}

pub struct PersistedSidebar {
  pub selected_index: usize,
  pub scroll: usize,
  pub expanded_schemas: Vec<String>,  // reserved for collapsible-schemas follow-up
}

pub enum PersistedTabKind { SqlEditor }

pub enum SaveOutcome { Canonical(PathBuf), PerPid(PathBuf) }

pub enum PersistError { Io(_), TomlDecode(_), TomlEncode(_), UnsupportedSchema { found, supported } }

pub fn load_at_start(path: &Path) -> PersistResult<Option<PersistedWorkspace>>;
pub fn save_at_exit(snapshot: &PersistedWorkspace, path: &Path) -> PersistResult<SaveOutcome>;
pub fn snapshot(core: &AppCore) -> PersistedWorkspace;
pub fn apply(core: &mut AppCore, snapshot: PersistedWorkspace,
  settings: &WorkspacePersistSettings) -> Option<String>;
```

No removals. The existing `Tab`, `UiState`, `SessionState` types
stay non-serde — they hold `Arc`s, channels and the raw
`tree_sitter::Parser` C handle that can never round-trip safely.
`PersistedWorkspace` is a separate **projection** layer; the
projection / restore lives in `crates/narwhal-app/src/core/persist_hook.rs`
because it needs direct access to `AppCore`'s private sub-states.

## Wire format

```toml
schema_version = 1
narwhal_version = "1.2.0"
saved_at = "1717423200s-since-epoch"
active_connection = "prod-pg"
active_tab = 1

[[tabs]]
name = "untitled-1"
buffer = "SELECT 1;"
cursor_row = 0
cursor_col = 7
scroll = 0
kind = "sql-editor"

[[tabs]]
name = "scratch"
buffer = ""
cursor_row = 0
cursor_col = 0
scroll = 0
kind = "sql-editor"

[sidebar]
selected_index = 3
scroll = 2
```

- `schema_version = 1` is always the very first key (matches the
  precedent set by `settings.toml` / `connections.toml`). The
  `Settings::peek_schema_version` cheap-path-first scanner isn't reused here because the file is tiny — the full TOML
  parse runs in microseconds on the snapshots we produce.
- Empty optionals (`narwhal_version`, `active_connection`,
  `sidebar.expanded_schemas`) collapse on serialisation. A
  first-run "just one untitled tab" install writes ~6 lines, not
  ~30.
- Enum variants serialise as kebab-case (`"sql-editor"`) so
  future additions read naturally in the TOML.

## Save / load triggers

| Trigger  | Where  | Notes  |
| -------------------- | -------------------------------------- | ------------------------------------------------ |
| **Load** at startup  | `App::with_workspace_state_path`  | Builder step, before `run`. Restored tabs land in `AppCore` before the first frame draws. |
| **Re-open** connection | `App::run`, before first draw  | Async `:open NAME` equivalent. Missing connection silently degrades to status-bar warning. |
| **Save** at clean exit | `App::run`, after the loop terminates | Panic unwinds skip this — the `Result` would have propagated and never reached the save call. |

The brief mentioned a throttled save every 30s while running. That
**deferred** to a follow-up: the atomic-rename guarantees the
file is either pre-snapshot or fully-current — never half-written
— and a clean-exit-only save reproduces the user mental model
("`:q` is what saved my session"). A 30s background save would
also have to avoid clobbering a clean exit that's mid-flight, which
needs additional coordination we'd rather not invent on this branch.

## Per-knob restore semantics

The `[settings.workspace.persist]` block has four bools, all
default-true:

| Field  | What happens when `false`  |
| ----------------- | ------------------------------------------------------------ |
| `enabled`  | Save *and* restore are no-ops. The file isn't created on exit; an existing file is left untouched on launch. |
| `restore_tabs`  | Default tab list (`untitled-1`) stays put. The active connection still re-opens (it's the single most-valuable restore item). |
| `restore_cursor`  | Buffer text restores; every tab reopens at `(row 0, col 0, scroll 0)`. Users who want "reopen my queries but start me at the top of each" pick this. |
| `restore_sidebar` | Sidebar selection and scroll start at zero on launch instead of replaying the saved offsets. |

`apply` returns the saved connection name (or `None`) so the
binary can fire `:open NAME` once the event loop is alive — keeping
the connection re-open off the critical path of the first frame.

## Treesitter cache interaction

The persist module **never touches** `Tab::ts_parser` or
`Tab::sql_highlights`. Those fields stay `None` on a restored tab,
which means the first render after restore re-runs the treesitter
parse — exactly the same path a freshly-typed buffer takes.

This satisfies the
`docs/dev/treesitter.md::"Cache policy"` invariant
(length-keyed cache, recompute on length mismatch): a freshly-restored
tab has `sql_highlights_buf_len = 0`, the loaded buffer has length
> 0 in 99 % of cases, so the cache is correctly invalidated on
first use. A restored *empty* buffer also re-computes because
`sql_highlights.is_none` short-circuits the cache-hit branch.

Raw `tree_sitter::Parser` handles wouldn't round-trip anyway — they
hold a C pointer through `tree_sitter::Parser` — so the policy of
"projection-only persistence, runtime caches rebuild on demand"
is both correct *and* the only thing that compiles.

## Concurrent narwhal instances

Two narwhal processes pointed at the same `workspace-state.toml`
would race the rename. We coordinate them with a `.lock` sibling
file using POSIX-atomic `OpenOptions::create_new`:

1. Writer A takes the lock, writes the canonical file via
  atomic-rename, removes the lock.
2. Writer B sees the lock, falls back to
  `workspace-state.${pid}.toml` so neither instance loses its
  snapshot.
3. A subsequent clean exit that acquires the lock takes over the
  canonical slot; the per-pid file stays on disk as a recoverable
  last-resort copy. The backlog has an item to age-out
  per-pid files at startup; v2.0 leaves them indefinitely.

Stale-lock recovery: lock files older than 60 seconds on disk are
treated as orphans from a crashed earlier run and reaped on the
next save attempt. The cutoff is comfortably longer than any
legitimate snapshot write (the snapshot is KB-class; the rename
takes milliseconds) but short enough that a normal restart
re-establishes the canonical path.

We considered pulling in `fs2` for proper advisory file locks
(`flock(2)` on Unix, `LockFileEx` on Windows) but rejected the
extra workspace dep — the `.lock`-sentinel scheme handles the
real-world case (two TUI processes opened in adjacent terminals)
correctly, and the cross-platform behaviour of `fs2` is the same
sentinel-with-extra-steps once you account for NFS / SMB
edge cases.

## Privacy

The snapshot file contains raw editor buffers. Users who paste
secrets into the editor (`SELECT * FROM users WHERE token = '…'`)
get those secrets written to disk in plaintext. Mitigations:

- The file is created with mode `0o600` on Unix (same as
  `settings.toml` / `connections.toml`). We set the mode on the
  temp file *before* the rename, so the canonical file is never
  briefly readable by other users.
- The Persist module-level rustdoc spells out the plaintext
  guarantee so security-sensitive users discover the opt-out via
  `cargo doc`.
- Users on shared machines can either:
  - flip `settings.workspace.persist.enabled = false`, or
  - point `XDG_CONFIG_HOME` at an encrypted directory.

## Tricky bits encountered

- **`Tab::editor` is private** but `editor_mut` is `const fn`.
  `EditorBuffer::set_cursor` / `set_scroll` are the canonical
  restore path; `set_cursor` snaps the column to a char boundary
  and clamps the row, so a malformed snapshot can't panic the
  restore.
- **`WorkspacePersistSettings::default` used to be all-false**. shipped the struct as a stub with derived `Default`, and
  the migration test asserted `!loaded.workspace.persist.enabled`.
  Flipping the defaults to "all true" needed a manual `Default`
  impl plus the matching assertion update in
  `crates/narwhal-config/tests/migrate_v1_to_v2.rs`.
- **`#[non_exhaustive]` blocks struct literals from external
  crates**. The integration test in `tests/persist_roundtrip.rs`
  was bitten by this. Followed the existing
  `ConnectionParams::with(|p| …)` convention from
  `docs/dev/api-surface.md` — every `non_exhaustive` struct in
  this module ships a `with` builder.
- **`active_tab` clamp** has to happen *after* the new tab list is
  installed, not before. The pre-install length is the *old*
  (default-one-tab) length; using it gives off-by-N indices when
  the snapshot has many tabs.
- **No `chrono` dep** in `narwhal-app`. The snapshot's `saved_at`
  field uses the unix-epoch-seconds shorthand instead. A future
  task that wants real ISO-8601 here can pull `chrono` in
  cheaply — the wire field is already `Option<String>`.
- **Tests that simulate lock contention** must use the *exact*
  per-pid path the production code computes. `persist::paths::per_pid_path`
  and `persist::paths::lock_path` are pub for this reason; without
  them the test would have to duplicate the path-construction
  heuristic and silently desync if the format ever changes.

## Acceptance criteria status

| Item  | Status |
| ------------------------------------------------------------- | :----: |
| `PersistedWorkspace` serde round-trips (golden TOML fixture)  |  ✅  |
| Clean exit writes the file atomically (rename, no temp left)  |  ✅  |
| Throttled save every 30s while running  |  ⏸ deferred to Tier 2 (see *Save / load triggers*) |
| Restore reopens the last active connection  |  ✅  |
| Restore rebuilds all tabs with correct cursor + scroll  |  ✅  |
| Sidebar viewport state restored  |  ✅  |
| Concurrent narwhal instances handled (lock fallback to per-pid) | ✅ |
| `settings.workspace.persist.enabled = false` → no file written |  ✅  |
| Schema version mismatch returns `UnsupportedSchema` and skips |  ✅  |
| `--all-targets` clippy + rustdoc + tests pass  |  ✅  |

## Out of scope

- **Throttled background save**: see *Save / load triggers* above.
- **Result-tab persistence**: re-running on demand is cheap;
  persisting 100k cached rows is not. `PersistedTabKind` is
  `non_exhaustive` so a future task that wants this can land it
  without a schema bump (just a new variant + projection arm).
- **Selection / multi-cursor state**: will extend the
  schema once `narwhal-vim` surfaces a stable selection model.
- **Plugin state**: plugins do their own storage by convention.
- **Cross-machine sync** (git-of-dotfiles style): users can symlink
  the file themselves; narwhal core doesn't ship sync.
- **Collapsible sidebar schemas**: the `expanded_schemas` field is
  reserved but unused in v2.0. Wired in the snapshot so the
  follow-up that adds collapse/expand doesn't have to bump the
  schema version.

## References

- `docs/dev/api-surface.md` — `non_exhaustive` + `with(|p| …)`
  builder pattern.
- `docs/dev/treesitter.md` — `Tab::sql_highlights` cache
  policy that persist must not violate.
- `narwhal_config::settings::atomic_write` — the precedent we
  mirror in `persist::paths::atomic_write`.
- DataGrip / VS Code workspace-state files as design references.

## Commit message template

```
feat(app): workspace persist — restore tabs / cursor / sidebar on launch

Snapshot the active connection, every open tab (buffer, cursor,
scroll), and sidebar state on clean exit. Restore on next launch
when settings.workspace.persist.enabled = true (the v2.0 default).

File: ~/.config/narwhal/workspace-state.toml — plaintext TOML,
0o600 on Unix, atomic-rename writes, .lock sentinel for concurrent
instances (per-pid fallback on contention). of v2.0 roadmap.
```
