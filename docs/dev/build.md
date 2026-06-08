# Building from source

## Prerequisites

- Rust **1.85+** (edition 2024)
- A C / C++ toolchain (for the bundled DuckDB and SQLite builds)
- `cmake` (DuckDB)
- `pkg-config` and `libdbus-1-dev` on Linux if you build without
  the `vendored` keyring feature

```sh
rustup install stable
rustup default stable
```

## Clone

```sh
git clone https://github.com/Nonanti/narwhal
cd narwhal
```

## Build

A debug build of the full workspace:

```sh
cargo build --workspace
```

A release binary with every driver wired in:

```sh
cargo build -p narwhaldb --release
./target/release/narwhal --version
```

A minimal SQLite-only build:

```sh
cargo build -p narwhaldb --release \
  --no-default-features \
  --features driver-sqlite
```

See [`../ARCHITECTURE.md`](../ARCHITECTURE.md) for the full driver
feature matrix.

## Run

```sh
cargo run -p narwhaldb
```

## CI gates

The repository's CI requires four checks. Run them locally before
opening a PR:

```sh
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
RUSTDOCFLAGS='-D warnings' cargo doc --workspace --no-deps
cargo test --workspace
```

## Nix

A `flake.nix` is committed at the repository root.

```sh
nix develop                      # dev shell with rustc + cargo + cmake
nix build                        # build the release binary
```

`direnv` is supported via the bundled `.envrc`.

## Cross-compilation

aarch64 Linux requires `cross` or a custom sysroot because the
DuckDB C++ tree does not cross-compile cleanly with the default
`x86_64` host headers. The release workflow targets x86_64 Linux,
x86_64 macOS, and aarch64 macOS natively on GitHub-hosted runners.
