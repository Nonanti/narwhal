# Plan 07 — v1.0 ship roadmap

## Goal

Take narwhal from "feels good in a smoke test" to "I'd ship this
to a teammate and not apologise". Plan 06 covered DataGrip parity
on the in-flight editing & input axis — Plan 07 closes the gaps
on the output, metadata, stability, and distribution axes.

The exit criterion for Plan 07 is concrete: a developer can clone
the repo, `cargo install`, point it at their day-to-day Postgres
or MySQL, and use narwhal as their primary client for a week
without bouncing back to DataGrip / DBeaver / psql.

Each item below gets its own detailed plan file (`plans/07-XX-*.md`)
once the design is locked; this document is the index, the
rationale, and the batch plan.

## Hard scope decisions (in / out)

### In (this plan)

- Output: export, row-detail view, multi-statement output, streaming
  feedback.
- Metadata: schema refresh, DDL generation, saved queries.
- Stability: TLS, byte-accurate driver tests across all 5 drivers,
  Lua plugin execution timeout.
- Distribution: README with screenshots/keymap, cargo install
  path verified, basic packaging notes.

### Out (deferred to v1.1)

- **SSH tunnel** — `libssh2` / `ssh2` adds heavy build-time deps
  and platform variance; v1.0 ships TLS-only.
- **Vim power-user features** — visual block (`Ctrl-V`), macros
  (`q<reg>` / `@<reg>`), marks (`m<a-z>`). Common enough to want
  but not common enough to block ship; v1.1.
- **Result column resize / hide** — requires the result table
  widget to track per-column width state and a mouse-drag /
  keyboard binding for resize. Defer.
- **Editor split view** — large refactor in the layout engine,
  not on the critical path.
- **Buffer autosave / crash recovery** — important but post-v1.0.
- **Profiler / EXPLAIN tree view** — `explain_cost.lua` already
  exists as a plugin; promote it to core only when the parse path
  is per-driver (postgres `EXPLAIN (FORMAT JSON)`, mysql `EXPLAIN
  FORMAT=tree`, etc.). Out of v1.0 scope.

## Work items

### Batch A — Output (parallel × 4)

These four items touch the result pane and the dispatch pipeline
but don't overlap with each other.

#### 07-01. Result export (CSV / JSON / INSERT)

**Why.** The `:export csv path` prompt already tab-completes
(plan 06-09) but the actual writer doesn't exist. `csv_export.lua`
plugin exists as a copy-row-to-clipboard helper but isn't a
real bulk export. Users currently can't get a query result out
of narwhal to disk.

**Scope.** Core `:export <fmt> <path>` command. Formats:
- `csv`  — RFC 4180, header row, NULLs as empty
- `json` — array of objects, NULLs as `null`
- `insert` — `INSERT INTO <table> (cols) VALUES (...);` per row
            (only valid when the result set has a known source
             table; status message otherwise)

Streams to disk; doesn't materialise the whole result in memory.
Respects an open filter / sort from plan 06-04.

#### 07-02. Row detail / form view

**Why.** The cell popup (Enter) shows one cell's value. For wide
tables with 20+ columns, reading a single row across the grid is
unworkable — DataGrip's "Row Editor" form is the standard
solution.

**Scope.** Capital `R` (or Shift+Enter) on the focused row opens
a key-value form modal: column name → value, scrollable, with
the same multi-line cell rendering the popup uses. Esc closes.
v1 read-only — editing rows from the form is v1.1.

#### 07-03. Multi-statement output tabs

**Why.** Running `SELECT 1; SELECT 2;` today shows… one of the
results, and the other vanishes. Multi-statement scripts are
common (migrations, comparison queries) and the user needs to
see every output.

**Scope.** When the dispatch pipeline produces N result sets, the
result pane gains a small tab strip (1/N · 2/N · ...) and the
user pages between them with `]r` / `[r` (or `Ctrl-PgDown` /
`Ctrl-PgUp`). Each tab carries its own ResultView state. Status
bar shows `result 2 of 5` so the user always knows.

#### 07-04. Streaming live row counter

**Why.** F7 streams a query without showing progress; for a
million-row export the user has no idea whether anything is
happening.

**Scope.** The streaming result pane title shows
`streaming · 12.3k rows · 2.1s` — updated on each chunk arrival.
Throttle to ≤10Hz so the render loop doesn't drown in updates.

### Batch B — Metadata & memory (parallel × 3)

#### 07-05. Schema cache refresh

**Why.** Sidebar tables are loaded once on connect and stale
forever after; users running CREATE/DROP DDL see a sidebar that
no longer matches the database. `:refresh` doesn't exist.

**Scope.** Explicit `:refresh` command (re-fetches the schema
catalogue for the active connection) plus auto-refresh after any
DDL-class statement (CREATE / DROP / ALTER / TRUNCATE / RENAME)
runs successfully. Auto-refresh debounced so a migration script
with 50 DDL statements doesn't fetch 50 times.

#### 07-06. DDL generation

**Why.** "Show me the CREATE statement for this table" is a
five-times-a-day operation. Today it requires typing
`SHOW CREATE TABLE x` (mysql) or hand-crafting an
`information_schema` query (postgres). The sidebar already knows
the table — give it a binding.

**Scope.** With a sidebar table focused, `d` (for "DDL") injects
the per-driver CREATE statement into the editor at the cursor
and runs it. Per-driver dispatch:
- postgres: `pg_dump --schema-only --table=...` via the
            information_schema (no shell-out — driver fetches
            columns + indexes + constraints and reconstructs the
            DDL textually)
- mysql:    `SHOW CREATE TABLE <qualified>`
- sqlite:   `SELECT sql FROM sqlite_master WHERE name = ?`
- duckdb:   `SELECT * FROM duckdb_tables() WHERE table_name = ?`
            + column dump
- clickhouse: `SHOW CREATE TABLE <qualified> FORMAT TabSeparated`

#### 07-07. Saved queries / bookmarks

**Why.** History (06-05) is a flat journal of everything you
ran; "save this exact query under a name" is a different need.
Users want a small, hand-curated library of queries they reach
for often.

**Scope.** `:save <name>` writes the current editor buffer under
`~/.config/narwhal/snippets/<name>.sql`. `:open <name>` (or its
tab-complete variant from 06-09 extended to snippets) loads it
back into a new tab. `:snippets` opens a modal list, Enter loads.

### Batch C — Stability (parallel × 3)

#### 07-08. TLS / SSL connection options

**Why.** Production Postgres and MySQL refuse plain-TCP. narwhal
currently has no TLS configuration — the connection wizard
doesn't expose `sslmode`, `ssl-cert`, etc.

**Scope.** Per-driver TLS options on `Connection` struct:
- `ssl_mode`: disable | prefer | require | verify-ca | verify-full
- `ssl_root_cert`: optional Path
- `ssl_cert`: optional Path
- `ssl_key`: optional Path

The wizard gains a "TLS" sub-page; the TOML schema gets the same
fields. Drivers wire the options into their respective TLS
configuration types (rustls / native-tls per the driver crate's
existing dependency choice — don't introduce a new TLS stack).

#### 07-09. Byte-accurate row tests for every driver

**Why.** Plan 05 ported ClickHouse to byte-accurate row mapping
with a thorough test set. The other four drivers (postgres,
mysql, sqlite, duckdb) don't have the same coverage — invalid
UTF-8, embedded NULs, NULL vs empty-string disambiguation, tab/
newline-in-string, and CRLF round-tripping are all untested.

**Scope.** For each of postgres, mysql, sqlite, duckdb: add a
test module that asserts:
- NULL vs empty string distinction
- invalid UTF-8 surface as `Value::Bytes` (not lossy String)
- embedded `\0` survives round-trip
- tab/newline-in-string survive
- numeric edge cases (i64::MAX, f64 NaN/Inf rejection)

Tests run against a real DB in CI when available, skip otherwise
(use the existing `narwhal-tests-*` feature gating).

#### 07-10. Plugin Lua execution timeout

**Why.** A plugin with `while true do end` locks the entire TUI
forever — there's no interrupt mechanism. Plugin authors are
also users, so this is reachable in normal use.

**Scope.** `mlua` exposes an `interrupt` callback that fires
periodically. Hook it; on N elapsed seconds of plugin execution
(configurable, default 5s) raise a Lua error that the host
catches and renders as a status message. Plugins that legitimately
need longer (e.g. a streaming explain) opt out via a
`narwhal.set_timeout(seconds)` global.

### Batch D — Docs & distribution (sequential × 2)

These two are sequential because the README pulls screenshots
from the latest binary, which the distribution work also touches.

#### 07-11. README with screenshots, GIF, keymap reference

**Why.** The current README is a one-liner. A potential user
landing on the GitHub page has no idea what narwhal looks like
or what it does that psql / DataGrip don't.

**Scope.** README rewrite covering:
- Hero GIF (asciinema → optimised gif)
- "Why narwhal" — three-bullet pitch
- Install (cargo install, AUR, build-from-source)
- Quick start (open wizard, run a query, F1 cheatsheet)
- Keymap reference table (mirrors 06-08's CHEATSHEET content)
- Plugin section (link to `examples/plugins/`, short API note)
- Architecture diagram (workspace crates, dispatch flow)

Screenshots taken from the latest release binary, dropped in
`docs/img/`.

#### 07-12. Distribution path

**Why.** `cargo install --git ...` works today but isn't
discoverable; there's no `narwhal` on crates.io and no packaging
for any distro.

**Scope.**
- Publish workspace crates to crates.io in dependency order
  (narwhal-core → drivers → narwhal-app → narwhal binary).
- Update README's install section to `cargo install narwhal-cli`
  (or whatever the binary crate ends up being called).
- AUR `PKGBUILD` template under `packaging/aur/` (don't submit;
  document how a downstream packager would).
- Homebrew formula template under `packaging/homebrew/` (same
  deal).
- Verify the `cargo install` path on a fresh shell without the
  nix flake — the dependency closure must be reachable from
  stable rustc.

## Batches & sequencing

| Batch | Items                              | Parallel? | Worktree? |
|-------|------------------------------------|-----------|-----------|
| A     | 07-01, 07-02, 07-03, 07-04         | 4-way     | yes       |
| B     | 07-05, 07-06, 07-07                | 3-way     | yes       |
| C     | 07-08, 07-09, 07-10                | 3-way     | yes       |
| D     | 07-11 → 07-12                      | sequential| n/a       |

After every batch: manual review of each agent's output, a
bug-fixer pass to close the cracks (every prior batch has
surfaced 3-5 critical issues the agent missed in error paths),
then squash into clean single commits.

Expected test delta:

| Item   | Tests added |
|--------|-------------|
| 07-01  | +6 |
| 07-02  | +4 |
| 07-03  | +5 |
| 07-04  | +3 |
| 07-05  | +5 |
| 07-06  | +5 |
| 07-07  | +5 |
| 07-08  | +4 |
| 07-09  | +12 (3 per driver × 4 drivers) |
| 07-10  | +3 |
| 07-11  | 0 (docs) |
| 07-12  | 0 (packaging) |
| **Total** | **+52** |

Baseline (after Plan 06 fully complete) ≈ 256 tests; after Plan
07 ≈ 308 tests.

## Acceptance for v1.0

When Plan 07 is fully merged, the project has:

- Every Plan 06 + Plan 07 item shipped on `main`.
- `cargo install narwhal-cli` works from a fresh shell.
- README with screenshots, install instructions, keymap.
- CI green on all 5 drivers' byte-accurate tests.
- A version tag `v1.0.0` cut on `main`, release artifacts
  attached.

At that point the project is "done" in the sense that it's
shippable; v1.1 starts with the deferred items above.
