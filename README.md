# narwhal

A terminal-first database client. Modal editing, multi-engine, single binary.

## Status

Pre-alpha. The workspace skeleton compiles, the modal input layer is unit
tested, and the PostgreSQL and SQLite drivers establish connections.
Statement execution and schema introspection are stubbed pending
implementation.

## Goals

- First-class modal editing across every interactive surface.
- Engine-agnostic core: adding support for a new database means writing a
  new driver crate and registering it at start-up.
- Zero accidental data loss: every destructive operation requires explicit
  confirmation, and statements are journaled.
- Predictable performance on large result sets through server-side
  pagination and streaming.

## Architecture

```
narwhal-core              public traits, value model, errors
narwhal-config            on-disk configuration, OS keyring integration
narwhal-driver-postgres   PostgreSQL implementation
narwhal-driver-sqlite     SQLite implementation
narwhal-vim               modal keystroke processor
narwhal-tui               ratatui-based interface
narwhal-app               event loop, driver registry, terminal lifecycle
narwhal                   binary entry point
```

## Building

A `flake.nix` is provided for NixOS hosts:

```sh
nix develop
cargo build --release
```

On other systems any toolchain at or above the version pinned in
`rust-toolchain.toml` will do.

## Licence

Dual-licensed under the [MIT](LICENSE-MIT) and
[Apache 2.0](LICENSE-APACHE) licences. Contributions are accepted under the
same terms.
