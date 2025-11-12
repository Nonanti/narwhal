# Releasing narwhal

This document describes the release procedure for narwhal.
The binary crate is published under **`narwhal-cli`** on crates.io
to avoid name collisions; the installed binary remains `narwhal`.

## 1. Cut the version

- Bump `workspace.package.version` in the root `Cargo.toml`.
- Update `CHANGELOG.md`.

## 2. Commit and tag

```sh
git commit -am "chore: release v1.0.0"
git tag -s v1.0.0 -m "v1.0.0"
```

## 3. Publish to crates.io (in dependency order)

```sh
cargo publish -p narwhal-core
cargo publish -p narwhal-config
cargo publish -p narwhal-sql
cargo publish -p narwhal-pool
cargo publish -p narwhal-history
cargo publish -p narwhal-driver-postgres
cargo publish -p narwhal-driver-mysql
cargo publish -p narwhal-driver-sqlite
cargo publish -p narwhal-driver-duckdb
cargo publish -p narwhal-driver-clickhouse
cargo publish -p narwhal-plugin
cargo publish -p narwhal-plugin-lua
cargo publish -p narwhal-vim
cargo publish -p narwhal-tui
cargo publish -p narwhal-app
cargo publish -p narwhal-cli
```

## 4. Build release artifacts

```sh
cargo build --release --bin narwhal
```

- Tar artifacts per platform.
- Sign with cosign or GPG.

## 5. Push the tag

```sh
git push origin v1.0.0
```

## 6. Update packaging templates

- Bump `pkgver` in `packaging/aur/PKGBUILD`.
- Bump `url` + `sha256` in `packaging/homebrew/narwhal.rb`.
- Open PRs / AUR submissions.
