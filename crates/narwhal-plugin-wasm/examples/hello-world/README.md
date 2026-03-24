# hello-world — example narwhal WASM plugin

The simplest possible plugin: logs a line for every lifecycle event
narwhal emits.

## Build

```bash
# One-time: install the component-model toolchain.
cargo install cargo-component

cd crates/narwhal-plugin-wasm/examples/hello-world
cargo component build --release
```

Output: `target/wasm32-wasip1/release/hello_world.wasm`.

## Run against the host

This crate is **not** part of the workspace (its `wit-bindgen`
dependency may not match the host's `wasmtime` pinning). Build it
stand-alone, then point the host's integration test at it:

```bash
NARWHAL_WASM_EXAMPLE="$(pwd)/target/wasm32-wasip1/release/hello_world.wasm" \
  cargo test -p narwhal-plugin-wasm --test runtime -- --ignored \
  end_to_end_event_delivery_with_real_component
```

For production use, copy the `.wasm` and the `plugin.toml` to
`$XDG_CONFIG_HOME/narwhal/plugins/wasm/hello-world/` and enable the
runtime via `~/.config/narwhal/config.toml`:

```toml
[plugins.wasm]
enabled = true
```
