# Plan 07-05 — Schema cache refresh

## Why

Sidebar tables are loaded once on `:open` and stale forever
after. Users running CREATE/DROP DDL — migrations, scratch
tables, ad-hoc additions — see a sidebar that no longer matches
the database, and the only fix is to disconnect and reconnect.

The schema list is also the input for completion (06-03), so a
stale cache means completion suggests dropped tables.

## Scope

- Explicit `:refresh` command re-fetches the schema catalogue
  for the active connection.
- Auto-refresh after any DDL-class statement runs successfully:
  CREATE, DROP, ALTER, TRUNCATE, RENAME (case-insensitive,
  first-token match).
- Auto-refresh is **debounced**: a migration script with 50 DDL
  statements in a single batch fires the refresh once at the end
  of the batch, not 50 times.
- Status message: `schema refreshed · 12 tables` (or whatever
  the new count is).

## Constraints

- AGENTS.md: no `unwrap()` / `expect()` in production code.
- `nix develop --command cargo fmt --all -- --check` clean.
- `nix develop --command cargo clippy --all-targets -- -D warnings` clean.
- One conventional commit, long-form.
- The schema catalogue fetch is async and per-driver; reuse
  whatever path `connect()` already calls.

## Concrete steps

### Step 1: locate the existing schema fetch

`AppCore` already populates `self.sidebar` somewhere on `:open`.
Find that code path (likely `connect_to_named`,
`build_schema_catalogue` or similar). Extract it into a method:

```rust
async fn refresh_schema(&mut self) -> Result<()> {
    let Some(conn) = self.connection.as_ref() else {
        self.status.message = "no active connection".into();
        return Ok(());
    };
    let catalogue = conn.fetch_schema().await?;
    let table_count = catalogue.total_tables();
    self.sidebar = build_sidebar_from(catalogue);
    self.status.message = format!("schema refreshed · {table_count} tables");
    Ok(())
}
```

### Step 2: `:refresh` command

`commands.rs`:
```rust
Command::Refresh,
```

`run_command` branch calls `self.refresh_schema().await`.

Add to `BUILTIN_COMMAND_NAMES` and the description table so
06-08's help panel picks it up.

### Step 3: DDL classifier

```rust
fn is_ddl_statement(sql: &str) -> bool {
    let head = sql.trim_start().split_whitespace().next()
        .unwrap_or("")
        .to_ascii_uppercase();
    matches!(head.as_str(),
        "CREATE" | "DROP" | "ALTER" | "TRUNCATE" | "RENAME")
}
```

### Step 4: debounced auto-refresh

`run.rs::dispatch_batch` runs N statements. Set a flag while
iterating:

```rust
let mut needs_refresh = false;
for stmt in statements {
    let result = run_one(stmt).await?;
    if result.ok && is_ddl_statement(&stmt.sql) {
        needs_refresh = true;
    }
}
if needs_refresh {
    self.schedule_schema_refresh();
}
```

`schedule_schema_refresh` debounces with a `tokio::time::sleep`
spawned task that resets on each new schedule call:

```rust
fn schedule_schema_refresh(&mut self) {
    self.refresh_pending.store(true, Ordering::Relaxed);
    // Drop the previous task if any
    if let Some(handle) = self.refresh_task.take() {
        handle.abort();
    }
    let tx = self.run_tx.clone();
    let pending = self.refresh_pending.clone();
    self.refresh_task = Some(tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(200)).await;
        if pending.swap(false, Ordering::Relaxed) {
            let _ = tx.send(RunUpdate::SchemaRefresh).await;
        }
    }));
}
```

`handle_run_update` then catches `RunUpdate::SchemaRefresh` and
calls `refresh_schema().await`.

### Step 5: tests

`tests/schema_refresh.rs`:

1. `manual_refresh_repopulates_sidebar` — open against sqlite,
   call `:refresh`, assert sidebar count matches catalogue.
2. `create_table_triggers_auto_refresh` — run CREATE TABLE,
   wait for debounce, assert sidebar includes the new table.
3. `drop_table_triggers_auto_refresh` — DROP TABLE, assert
   sidebar no longer includes the dropped name.
4. `non_ddl_no_refresh` — run a plain SELECT, assert no refresh
   was scheduled.
5. `batched_ddl_debounces` — run a single batch with three
   CREATE TABLEs, assert exactly one refresh executed.

Acceptance: +5 tests.

## Files

- `crates/narwhal-app/src/core.rs` (refresh_schema,
  schedule_schema_refresh, refresh_task, refresh_pending,
  RunUpdate::SchemaRefresh, handle_run_update branch)
- `crates/narwhal-app/src/commands.rs` (Command::Refresh,
  BUILTIN_COMMAND_NAMES entry)
- `crates/narwhal-app/src/run.rs` (DDL classifier, dispatch_batch
  flag)
- `crates/narwhal-tui/src/widgets/help.rs` (CHEATSHEET entry for
  :refresh)
- `crates/narwhal-app/tests/schema_refresh.rs` (new)

## Acceptance

- `nix develop --command cargo fmt --all -- --check` clean
- `nix develop --command cargo clippy --all-targets -- -D warnings` clean
- `nix develop --command cargo test --all` reports +5 from baseline
- Manual smoke: CREATE a table, watch the sidebar update within
  200-300ms; run `:refresh` explicitly, sidebar updates +
  status bar reports.

## Commit message template

```
feat(app): :refresh + auto schema reload on DDL

Sidebar tables were loaded once on :open and stale forever after.
Users running CREATE / DROP / ALTER migrations had to disconnect
and reconnect to see the new shape of their database — and
completion (plan 06-03) kept suggesting tables that no longer
existed.

Two paths now reload the schema catalogue:

- :refresh   explicit command, fetches the catalogue for the
             active connection, rebuilds the sidebar, reports
             the new total to the status bar.
- Auto      dispatch_batch flags every statement whose first
             token is CREATE / DROP / ALTER / TRUNCATE / RENAME
             (case-insensitive); if any flag is set when the
             batch finishes, schedule_schema_refresh kicks off
             a debounced refresh.

Debounce window: 200ms via a tokio::spawn'd sleep task that
re-arms on each schedule call. A migration with 50 DDL
statements fires exactly one refresh; an interactive CREATE
followed by ALTER 100ms later also fires once.

Five new tests cover :refresh, CREATE/DROP triggers, the
non-DDL no-op path, and the batched-debounce guarantee.
```
