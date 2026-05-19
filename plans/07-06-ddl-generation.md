# Plan 07-06 — DDL generation from the sidebar

## Why

"Show me the CREATE statement for this table" is a five-times-
a-day operation. Today it requires the user to type
`SHOW CREATE TABLE x` (mysql) or hand-craft an
`information_schema` query (postgres). The sidebar already
knows the table — it just has to expose a binding to fetch the
DDL.

## Scope

With a sidebar table focused, `d` (for "DDL") fetches the
table's CREATE statement, injects it into the editor at the
cursor (or replaces a blank scratch tab's buffer), and does **not**
auto-run — the user inspects it first.

Per-driver implementation:

- **sqlite**     `SELECT sql FROM sqlite_master WHERE type='table'
                 AND name = ?` — direct, sqlite stores the DDL
                 verbatim.
- **mysql**      `SHOW CREATE TABLE <qualified>` — returns the
                 DDL in column 1.
- **clickhouse** `SHOW CREATE TABLE <qualified> FORMAT TSV` —
                 returns the DDL string.
- **postgres**   No native `SHOW CREATE`. Reconstruct from
                 `pg_catalog`: query `pg_attribute`, `pg_constraint`,
                 `pg_index` for the columns / PKs / FKs / indexes
                 and assemble the DDL text. v1 emits `CREATE TABLE
                 ... ( ... );` with column list + PK + NOT NULL +
                 DEFAULT. v2 covers FKs / indexes / sequences.
- **duckdb**     `SELECT sql FROM duckdb_tables() WHERE table_name = ?`
                 — duckdb stores DDL similarly to sqlite.

Status message: `injected DDL for <schema>.<table>`.

## Constraints

- AGENTS.md: no `unwrap()` / `expect()` in production code.
- `nix develop --command cargo fmt --all -- --check` clean.
- `nix develop --command cargo clippy --all-targets -- -D warnings` clean.
- One conventional commit, long-form.
- Per-driver code lives in the respective `narwhal-driver-*`
  crate, exposed via a new trait method on the `Driver` trait.

## Concrete steps

### Step 1: `Driver::fetch_ddl`

`narwhal-core::driver` adds:

```rust
#[async_trait]
pub trait Driver: Send + Sync {
    // ... existing
    async fn fetch_ddl(&self, qual: &QualifiedName) -> Result<String, Error>;
}
```

Default impl returns an error variant so drivers that don't yet
implement it surface cleanly. Each of the 5 driver crates
overrides.

### Step 2: postgres reconstructor

Postgres needs the most work. Build it in
`narwhal-driver-postgres/src/ddl.rs`:

```rust
pub async fn build_create_table(
    client: &tokio_postgres::Client,
    qual: &QualifiedName,
) -> Result<String, Error> {
    // 1. Fetch columns from pg_attribute
    // 2. Fetch PK from pg_constraint where contype='p'
    // 3. Fetch NOT NULL flags from pg_attribute.attnotnull
    // 4. Fetch defaults from pg_attrdef
    // 5. Format as CREATE TABLE schema.name (
    //      col1 type [NOT NULL] [DEFAULT expr],
    //      ...
    //      PRIMARY KEY (cols)
    //    );
}
```

### Step 3: keystroke

`handle_sidebar_key`:

```rust
KeyCode::Char('d') => {
    if let Some(SidebarRow {
        kind: SidebarRowKind::Table { schema, name }, ..
    }) = self.sidebar.selected_row() {
        self.inject_ddl(schema, name).await;
    }
}
```

`inject_ddl`:
```rust
async fn inject_ddl(&mut self, schema: &str, name: &str) {
    let Some(driver) = self.active_driver() else {
        self.status.message = "no active connection".into();
        return;
    };
    let qual = QualifiedName::new(schema, name);
    match driver.fetch_ddl(&qual).await {
        Ok(ddl) => {
            let tab = &mut self.tabs[self.active_tab];
            tab.editor.insert_text_at_cursor(&ddl);
            self.status.message = format!("injected DDL for {qual}");
        }
        Err(e) => {
            self.status.message = format!("DDL fetch failed: {e}");
        }
    }
}
```

### Step 4: tests

`crates/narwhal-driver-{sqlite,postgres,mysql,duckdb,clickhouse}/tests/ddl.rs`
each gets one test that:
1. Creates a test table with a known DDL shape.
2. Calls `fetch_ddl`.
3. Asserts the returned string contains the table name + every
   column name.

The postgres test additionally asserts PK and NOT NULL survive.

Plus one `crates/narwhal-app/tests/ddl_inject.rs` for the
keybinding wiring:
- Press `d` with a sidebar table focused → editor contains the
  DDL string.

Acceptance: +5 tests (1 per driver) — postgres / mysql /
clickhouse tests skip if no test DB is configured (feature
gate); sqlite + duckdb always run.

## Files

- `crates/narwhal-core/src/driver.rs` (`fetch_ddl` trait method)
- `crates/narwhal-driver-sqlite/src/lib.rs`
- `crates/narwhal-driver-mysql/src/lib.rs`
- `crates/narwhal-driver-postgres/src/lib.rs`
- `crates/narwhal-driver-postgres/src/ddl.rs` (new)
- `crates/narwhal-driver-duckdb/src/lib.rs`
- `crates/narwhal-driver-clickhouse/src/lib.rs`
- `crates/narwhal-app/src/core.rs` (`handle_sidebar_key` binding,
  `inject_ddl`)
- `crates/narwhal-tui/src/widgets/help.rs` (CHEATSHEET entry for
  `d`)
- Five `tests/ddl.rs` files (one per driver crate)
- `crates/narwhal-app/tests/ddl_inject.rs` (new)

## Acceptance

- `nix develop --command cargo fmt --all -- --check` clean
- `nix develop --command cargo clippy --all-targets -- -D warnings` clean
- `nix develop --command cargo test --all` reports +5 from baseline
- Manual smoke against sqlite: select a table in the sidebar,
  press `d`, see `CREATE TABLE ...` in the editor.

## Commit message template

```
feat(drivers,app): d on a sidebar table fetches and injects DDL

"Show me the CREATE statement for this table" is a five-times-
a-day operation that previously required typing SHOW CREATE
TABLE x (mysql) or hand-crafting an information_schema query
(postgres).  The sidebar already knew the table name; now the
keybinding catches up.

Pressing d with a sidebar table focused calls Driver::fetch_ddl
(a new trait method) and injects the result into the editor at
the cursor.  Per-driver implementations:

- sqlite     SELECT sql FROM sqlite_master — verbatim
- mysql      SHOW CREATE TABLE — verbatim
- clickhouse SHOW CREATE TABLE … FORMAT TSV — verbatim
- duckdb     SELECT sql FROM duckdb_tables() — verbatim
- postgres   reconstructed from pg_catalog (pg_attribute,
             pg_constraint, pg_attrdef): CREATE TABLE schema.name
             with columns + NOT NULL + DEFAULT + PRIMARY KEY.
             FKs / indexes / sequences are a v1.1 follow-up.

The injection is non-destructive — the editor text the user was
composing is preserved and the DDL appears at the cursor.  No
auto-run; the user inspects and decides.

Five new tests (one per driver, postgres/mysql/clickhouse feature-
gated against a live test DB, sqlite + duckdb always-on) verify
that fetch_ddl returns a CREATE TABLE string mentioning every
column.  An app-level wiring test covers the keystroke → editor
injection path.
```
