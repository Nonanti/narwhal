# Async trait style — `Connection`, `DatabaseDriver`, `RowStream`, `CancelHandle`

> Status: **adopted in v2.0**. Replaces the v1.x `#[async_trait]`
> macro on the four core traits.

## Decision

The four canonical traits in `narwhal-core` use **native `async fn` in
trait** (RPITIT — *return position impl trait in trait*), with every
`async fn` desugared explicitly to `-> impl Future + Send`. Each one is
paired with a **dyn-safe sibling** that boxes the returned future so
the workspace can keep its `Box<dyn ...>` / `Arc<dyn ...>` sites.

| Sized API (RPITIT)  | Dyn-safe sibling  |
| ------------------------- | -------------------------- |
| `Connection`  | `DynConnection`  |
| `DatabaseDriver`  | `DynDatabaseDriver`  |
| `RowStream`  | `DynRowStream`  |
| `CancelHandle`  | `DynCancelHandle`  |

A blanket `impl<T: Connection + 'static> DynConnection for T { ... }`
(and the same for the other three) lives in `narwhal-core`, so any
type that implements the sized trait automatically gains the dyn
wrapper at zero source-level cost.

## Why two traits?

Native `async fn` in trait is **not dyn-compatible** — the returned
future has an existential type that can't fit in a vtable slot. So
this is illegal:

```rust
pub trait Connection {
  async fn execute(&mut self, sql: &str) -> Result<QueryResult>;
}

let conn: Box<dyn Connection> = …; // error E0038: not dyn compatible
```

The classical workaround is to box the returned future. That's exactly
what `#[async_trait]` did under the hood, and exactly what
`DynConnection` does today — but explicit, opt-in, and with one trait
shape per use case:

* **Driver authors / hot paths** implement `Connection` directly with
  `async fn` bodies. The compiler enforces `Send` on every returned
  future. No allocation per call.
* **Trait-object sites** (the driver registry, the connection pool,
  the CLI dispatcher) hold `Box<dyn DynConnection>` /
  `Arc<dyn DynDatabaseDriver>`. Each call still goes through one
  `Box<dyn Future>` allocation — the same cost the v1.x
  `#[async_trait]` macro paid — but the trait surface is hand-written
  so we can audit it.

## Concrete shape

```rust
// narwhal-core/src/connection.rs — sized
pub trait Connection: Send + Sync {
  fn execute(
  &mut self,
  sql: &str,
  params: &[Value],
  ) -> impl Future<Output = Result<QueryResult>> + Send;
  // …
}

// narwhal-core/src/connection.rs — dyn
pub trait DynConnection: Send + Sync {
  fn execute<'a>(
  &'a mut self,
  sql: &'a str,
  params: &'a [Value],
  ) -> BoxFuture<'a, Result<QueryResult>>;
  // …
}

impl<T> DynConnection for T
where
  T: Connection + 'static,
{
  fn execute<'a>(
  &'a mut self,
  sql: &'a str,
  params: &'a [Value],
  ) -> BoxFuture<'a, Result<QueryResult>> {
  Box::pin(<Self as Connection>::execute(self, sql, params))
  }
  // …
}
```

## Driver authoring rules

1. Implement `Connection` (or `DatabaseDriver`, `RowStream`,
  `CancelHandle`) directly. Use `async fn` bodies; the compiler
  handles the `impl Future + Send` desugaring.
2. **Do not** import `DynConnection` / `DynDatabaseDriver` etc. via
  `use` inside the driver crate. They're in scope as blanket impls
  anyway. Importing them brings duplicate methods into scope and
  causes `E0034 multiple applicable items in scope` at every
  intra-impl call (e.g. `self.execute(...)` inside a default-method
  override).
3. Reference the dyn traits by fully-qualified path in return types
  when you need to hand out a trait object:

  ```rust
  async fn connect(
  &self,
  config: &ConnectionConfig,
  password: Option<&str>,
  ) -> Result<Box<dyn narwhal_core::DynConnection>> { … }
  ```

## Consumer authoring rules

Crates that hold trait objects (`narwhal-pool`, `narwhal-app`,
`narwhal-mcp`, `narwhal-drivers`, the `narwhal` binary) **do**
import the `Dyn*` traits so the methods are in scope. They never
import the sized trait — there is no need, the dyn trait covers the
full API.

## Trade-offs we accepted

* Each trait surface is duplicated, roughly doubling the lines in
  `narwhal-core/src/{connection,driver,stream,cancel}.rs`. Acceptable
  one-time cost; the duplication is mechanical and never changes
  unless we add a trait method.
* The boxed-future shape is named once via
  `narwhal_core::BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T>
  + Send + 'a>>` (see `narwhal-core/src/future.rs`). Every dyn-safe
  method uses it, which keeps each method signature one line
  instead of three and lets us hold `clippy::type_complexity` at the
  workspace default (250). The recurring
  `Result<Vec<(Schema, Vec<Table>)>>` from `list_all_tables`
  similarly gets a name — `narwhal_core::SchemaCatalog`.
* Trait files set `#![allow(clippy::needless_lifetimes,
  clippy::elidable_lifetime_names)]` at module level. Every borrowed
  parameter on the dyn-safe methods shares one lifetime with the
  returned `BoxFuture`; elision would give each parameter an
  independent anonymous lifetime, which compiles but loses the
  contract that the future only lives as long as the shortest of
  the inputs. The explicit `'a` is documentation, not noise.
* `async-trait` is **removed** from `narwhal-core`'s `Cargo.toml` and
  from every driver's `Cargo.toml`. Other crates that still use
  `#[async_trait]` for *their own* unrelated traits
  (`narwhal-plugin`, `narwhal-config::CredentialStore`,
  `narwhal-mcp` tools, …) keep the dep; reshaping those is out of
  scope.

## Migration note for external implementors

If you previously had

```rust
#[async_trait]
impl Connection for MyDriver {
  async fn execute(&mut self, sql: &str, params: &[Value]) -> Result<QueryResult> { … }
  // …
}
```

drop the `#[async_trait]` attribute and update the `connect` /
`stream` / `cancel_handle` return types to the `Dyn*` siblings:

```rust
impl Connection for MyDriver {
  async fn execute(&mut self, sql: &str, params: &[Value]) -> Result<QueryResult> { … }
  async fn stream(
  &mut self, sql: &str, params: &[Value],
  ) -> Result<Box<dyn narwhal_core::DynRowStream>> { … }
  fn cancel_handle(&self) -> Option<Box<dyn narwhal_core::DynCancelHandle>> { … }
  // …
}
```

Everything else stays identical. The full diff is (migration
guide).
