# Plan 07-11 — README with screenshots, GIF, keymap reference

## Why

The current `README.md` is a one-liner. A potential user landing
on the GitHub page has no idea what narwhal looks like, what it
does that `psql` / DataGrip don't, or how to install it. That's
the first impression and it's selling the project short.

## Scope

A README rewrite covering:

1. **Hero GIF** — asciinema recording → optimised gif via `agg`.
   Shows: open wizard → connect to sqlite → run a query → cell
   popup → completion → `:export csv`.

2. **"Why narwhal" — three bullets**:
   - One TUI, five databases, no driver-juggling.
   - Vim editing + auto-pair + completion (so it doesn't feel
     like a 90s tool).
   - Lua plugins for the bits that should be yours.

3. **Install** section:
   - `cargo install narwhal-cli` (depends on 07-12 landing)
   - `nix run github:berkant/narwhal` (flake users)
   - Build from source (`git clone && cargo build --release`)
   - AUR / Homebrew links (post 07-12)

4. **Quick start** — three steps:
   - Run `narwhal`, hit `:open` to open the wizard.
   - Fill in driver + host + db.
   - F6 to run, F1 for help.

5. **Keymap reference table** — mirrors the CHEATSHEET from
   06-08, in three sections (Global / Editor / Results).

6. **Plugin section**:
   - Link to `examples/plugins/`.
   - Short API note pointing at the Lua globals.
   - Where plugins live (`~/.config/narwhal/plugins/`).

7. **Architecture diagram** — workspace crates and dispatch
   flow. ASCII art is fine; SVG in `docs/img/` is better.

8. **Status badges** — CI, license, version.

Screenshots / GIF live under `docs/img/`. Hero GIF target size
≤2MB so GitHub serves it inline; use `agg --speed 2 --idle-time-limit 1`.

## Constraints

- No code changes; this is purely docs.
- Conventional commit, long-form.
- Markdown must render correctly on GitHub (no fancy
  extensions).
- All image paths relative (`./docs/img/hero.gif`).

## Concrete steps

### Step 1: record the asciinema

```sh
asciinema rec -c 'target/release/narwhal' --idle-time-limit 1 docs/hero.cast
```

Script the demo: launch → `:open smoke` → `SELECT * FROM users;`
F6 → results pane → `R` row detail → `q` → `:export csv /tmp/out.csv`.

### Step 2: convert to GIF

```sh
agg --speed 2 --idle-time-limit 1 \
    --font-family "JetBrains Mono" --font-size 14 \
    docs/hero.cast docs/img/hero.gif
```

Verify the GIF is ≤2MB; otherwise re-record shorter.

### Step 3: take supplementary screenshots

PNG of:
- Wizard (`docs/img/wizard.png`)
- Result with completion popup (`docs/img/completion.png`)
- Help modal F1 (`docs/img/help.png`)
- Sidebar + DDL injection (`docs/img/ddl.png`)

Use a fixed 100×30 terminal size for consistency.

### Step 4: write the README

The new README structure:

```markdown
# narwhal

> A TUI database client that doesn't feel like the 90s.

![hero](./docs/img/hero.gif)

## Why narwhal

- One TUI, five databases (postgres, mysql, sqlite, duckdb,
  clickhouse), no driver-juggling.
- Vim editing with auto-pair, context-aware completion, and a
  proper command palette.
- Lua plugin runtime — the bits that should be yours, stay yours.

## Install

[…]

## Quick start

[…]

## Keymap

### Global

| Keys | Action |
|------|--------|
| F5 / Alt-Enter / Ctrl-; | Run statement under cursor |
[…]

### Editor

[…]

### Results

[…]

## Plugins

[…]

## Architecture

[…]

## Status

![CI](…) ![License](…) ![Version](…)
```

### Step 5: source-of-truth check

The keymap table must match `BUILTIN_COMMAND_DESCRIPTIONS` +
`CHEATSHEET` exactly. Add a CI step (or a `xtask`) that parses
the README's keymap table and diffs against the CHEATSHEET; fail
CI if they drift.

Skip the CI step in this plan if it requires substantial
infrastructure — the v1.0 release notes can mention it as a
v1.1 follow-up.

## Files

- `README.md` (rewrite, ~250 lines)
- `docs/img/hero.gif` (new)
- `docs/img/wizard.png` (new)
- `docs/img/completion.png` (new)
- `docs/img/help.png` (new)
- `docs/img/ddl.png` (new)
- `docs/hero.cast` (asciinema source, kept in repo for re-records)

## Acceptance

- `nix develop --command cargo fmt --all -- --check` clean
- `nix develop --command cargo clippy --all-targets -- -D warnings` clean
- `nix develop --command cargo test --all` reports same count
  (no test delta — this is docs).
- README renders correctly on GitHub (preview manually).
- Hero GIF plays inline + ≤2MB.

## Commit message template

```
docs(readme): screenshots, hero gif, keymap, install path

The README was a one-liner.  A potential user landing on the
GitHub page had no idea what narwhal looks like, what it does
that psql or DataGrip don't, or how to install it — and that's
the first impression the project gets.

Replace it with a structured doc:

- Hero GIF (./docs/img/hero.gif) recorded via asciinema and
  converted with agg; covers open-wizard → connect → run query →
  cell popup → completion → :export.
- "Why narwhal" three-bullet pitch (five databases, vim editing,
  Lua plugins).
- Install section: cargo install narwhal-cli (post-07-12),
  nix run, build-from-source, AUR + Homebrew references.
- Quick-start three-step (run, :open, F6).
- Keymap reference tables in three sections (Global / Editor /
  Results) — kept in sync with 06-08's CHEATSHEET.
- Plugin section linking to examples/plugins/ and noting where
  plugins live.
- Architecture diagram (ASCII workspace crate map).
- Status badges (CI, license, version).

Four supplementary screenshots under docs/img/ — wizard, the
completion popup, the F1 help modal, and DDL injection from the
sidebar.

No code changes.  The keymap-table-vs-CHEATSHEET drift CI guard
is noted as a v1.1 follow-up since wiring it would expand this
plan beyond a doc commit.
```
