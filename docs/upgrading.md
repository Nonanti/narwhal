# Upgrading

Most upgrades are drop-in. The exceptions and migration steps are
collected here per version.

## To 2.2.x

### Lua sandbox default flipped to `Restricted`

If you embed `narwhal-plugin-lua` and load scripts that need `io`,
`os`, `package`, `debug`, or `ffi`, opt them in explicitly:

```rust
use narwhal_plugin_lua::{LuaPlugin, LuaSandbox};

LuaPlugin::from_path_with_sandbox(path, LuaSandbox::Permissive)?;
```

User plugins from `~/.config/narwhal/plugins/` keep the safe
default. Scripts that import `io` or `os` will load but fail at
dispatch with `attempt to index a nil value (global 'io')`.

End users with auto-loaded plugins that need filesystem access:
remove and re-add them with a per-plugin manifest flag (planned
for a future release).

## To 2.1.x

No breaking changes. `[keybindings].vim_mode` is deprecated in
favour of `[editor].mode`, but the old field still round-trips for
back-compat.

## To 2.0.x

This is a major bump. Plan a config migration window.

### MSRV bumped to 1.85

Edition 2024 throughout. Bring your Rust toolchain up to 1.85 or
later:

```sh
rustup update stable
```

### Driver crates consolidated

The six per-backend crates folded into `narwhal-drivers` with one
cargo feature per backend. Downstream embedders update their
`Cargo.toml`:

```toml
# Before
narwhal-driver-postgres = "1.2"
narwhal-driver-sqlite = "1.2"

# After
narwhal-drivers = { version = "2.0", features = ["postgres", "sqlite"] }
```

### `Connection` trait is now RPITIT

The trait uses native `async fn` (return-position `impl Trait` in
traits). Downstream drivers no longer need `#[async_trait]`. If you
implement `Connection` yourself, drop the attribute and let the
trait shape speak for itself.

### Settings schema v2

`config.toml` v1 files load with deprecation warnings. Run

```sh
narwhal migrate-config
```

to rewrite them in v2 shape. A backup is saved alongside the
original.

### API surface audit

A handful of types that were public in v1 are now `pub(crate)`.
Build failures in downstream crates surface them immediately. The
full v1 → v2 surface delta lives in
[`dev/api-surface.md`](./dev/api-surface.md).

## To 1.2.x / 1.1.x

No breaking changes. Drop-in upgrades.

## To 1.0.x

First public release. Nothing to migrate from.

## See also

- [`../CHANGELOG.md`](../CHANGELOG.md) for the per-version delta
- [`configuration.md`](./configuration.md) for the current settings
  reference
