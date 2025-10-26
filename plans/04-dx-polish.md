# Plan 04 — DX polish: more sample plugins + `:help <command>`

## Why

`examples/plugins/` ships four samples (uppercase, format_json,
row_count, query_snippet). They cover the basic shape of the API but
leave the most-asked daily-driver patterns (export, query history,
quick result analysis) unaddressed. And the built-in `:help` command
prints a single static line — it doesn't help you discover anything,
let alone learn a specific command's arguments.

## Constraints

- Don't add framework features; build on what's already there. The
  plugin API is `narwhal.register_command`, `narwhal.register_transform`,
  `narwhal.sql_run`, and the CommandOutcome shapes (`{sql, append}`,
  `{status}`, string, nil).
- Each sample plugin must:
  - Have a clear single-purpose top-of-file doc comment with usage.
  - Quote/escape identifiers safely (mirror the `safe_ident()` /
    whitelist pattern from `row_count.lua` and `query_snippet.lua`).
  - Run end-to-end inside the existing
    `shipped_example_plugins_load_and_work` integration test (extend
    it; don't fork it).
- `:help <command>` must work for both built-in commands and
  plugin-registered commands. For plugins, the description is the
  string passed to `narwhal.register_command(name, description, fn)`.
- One commit per change, conventional, long-form. Three commits in
  this plan: two new samples + the `:help` work.
- `clippy --all-targets -- -D warnings` clean, `fmt --check` clean.

## Concrete steps

### 04a. New sample: `examples/plugins/csv_export.lua`

`:csv-export <table> <path>` — query the table, dump to CSV. Uses
`narwhal.sql_run("SELECT * FROM " .. safe_ident(table))` to pull
rows, then `io.open(path, "w")` to write a header line + one CSV row
per result row. Quote any cell containing `,`, `"`, `\n` with the
standard CSV doubling rule. Status bar reports `wrote N row(s) to
<path>`.

Edge cases: empty result set is a valid output (just the header
line). Path validation: refuse a path with `..` to avoid writing
outside the cwd by accident.

### 04b. New sample: `examples/plugins/explain_cost.lua`

`:explain-cost` — wraps the current editor buffer in `EXPLAIN
ANALYZE` and injects it back via `{sql = "EXPLAIN ANALYZE …",
append = false}`. Tiny, but a very common daily action — saves
re-typing the prefix every time.

For SQLite (no `EXPLAIN ANALYZE`), fall back to plain `EXPLAIN`. The
driver capability isn't exposed to Lua, so detect heuristically: if
the *first time* the wrapped statement is run it errors out, the
user can pick the right variant via a second command, `:explain-sqlite`.
(Keep it dumb; don't try to be too clever from Lua.)

### 04c. `:help <command>` lookup

Currently `AppCore::execute_command` matches `Command::Help` and
prints a static one-liner of built-in commands. Replace that with:

- `:help` (no arg): the existing one-liner.
- `:help <name>`:
  - Look up `<name>` against the built-in command names
    (`crate::commands::BUILTIN_COMMAND_NAMES`).
    - If present, print a per-command description from a new
      `BUILTIN_COMMAND_DESCRIPTIONS: &[(&str, &str)]` table next to
      it. Three or four lines: one per command. Keep descriptions
      short — they have to fit on the status bar.
  - Else, look up against the plugin registry (`self.plugins.plugin_for(name)`)
    and print the plugin's `CommandDescriptor.description`.
  - Else, print `unknown command: <name>`.

## Files

- `examples/plugins/csv_export.lua` (new)
- `examples/plugins/explain_cost.lua` (new)
- `examples/plugins/README.md` (extend the table)
- `crates/narwhal-app/src/commands.rs`
  (add `BUILTIN_COMMAND_DESCRIPTIONS`, `Command::Help` variant
  carries `Option<String>` arg)
- `crates/narwhal-app/src/core.rs`
  (`:help <name>` lookup logic)
- `crates/narwhal-app/tests/plugin.rs`
  (extend `shipped_example_plugins_load_and_work` so it exercises
  `:csv-export` and `:explain-cost`)
- `crates/narwhal-app/tests/...` — wherever the `Command::Help`
  parser test lives, update.

## Tests

- Extend `shipped_example_plugins_load_and_work`:
  - Load all six plugins.
  - Assert the editor injection from `:explain-cost`.
  - Assert `:csv-export items <tmpfile>` writes the expected three
    lines (header + two data rows in `core_with_items`'s seeded data).
- Add `help_with_builtin_arg_describes_it`: assert that `:help open`
  prints something containing the word "open".
- Add `help_with_plugin_arg_describes_it`: load `row_count.lua`,
  call `:help rc`, assert the status message contains the
  description from the registry.

Acceptance: total test count rises by **at least 3** (one extended,
two new). Pre-Plan-04 baseline is 193; final number depends on
whether Plans 01–03 land first (they add 1+2 → 196). Either way,
this plan adds ≥3.

## Acceptance

- `nix develop --command cargo clippy --all-targets -- -D warnings` clean.
- `nix develop --command cargo test --all` passes.
- The new samples are loadable from `auto_load_plugins` and don't
  break the existing 4 samples.

## Commit boundaries

Three separate commits, in this order:

1. `feat(commands): per-command :help lookup for built-ins and plugins`
2. `docs(examples): csv-export sample plugin`
3. `docs(examples): explain-cost sample plugin`

Each commit message: conventional, long-form, why not what.
