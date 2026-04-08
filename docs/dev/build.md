# Building from source

```sh
git clone https://github.com/Nonanti/narwhal.git
cd narwhal
cargo build --release          # binary at target/release/narwhal
```

Requirements:

- Rust ≥ 1.85 (edition 2024 — see `rust-toolchain.toml`)
- C++17 toolchain for the bundled DuckDB build
- Linux: `cmake`, `libclang-dev`, `pkg-config`, `libdbus-1-dev`
- macOS: `cmake` (`brew install cmake`)

## Nix

```sh
nix develop                    # pulls cmake, clang, libcxx, libclang
cargo build --release
```

## Slim builds

Drivers are feature-gated. The default `cargo install narwhaldb`
includes all six engines; build with a subset to slim the binary:

```sh
cargo build --release --no-default-features --features driver-postgres
```

Available driver features: `driver-postgres`, `driver-mysql`,
`driver-sqlite`, `driver-duckdb`, `driver-clickhouse`,
`driver-mssql`.

## Tests

```sh
cargo test --workspace
```

Driver integration tests are gated behind `#[ignore]` and require
Docker (Postgres, MySQL, SQL Server, ClickHouse testcontainers):

```sh
cargo test --workspace -- --include-ignored --test-threads=1
```

## Pre-push checklist

CI runs these four checks with `-D warnings`. Run them locally before
pushing to avoid red builds:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
RUSTDOCFLAGS='-D warnings' cargo doc --workspace --no-deps
cargo test --workspace
```
