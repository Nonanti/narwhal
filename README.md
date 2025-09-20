# 🐋 narwhal

A TUI database client — DataGrip in your terminal.

> ⚠️ **Status:** Pre-alpha. Skeleton only.

## Features (planned)

- 🔌 **Multi-database:** PostgreSQL, SQLite (MVP) → MySQL, Redis, MongoDB, MSSQL
- ⌨️  **Vim mode** as first-class citizen
- 🗂️  Schema browser (databases → schemas → tables → columns)
- 📝 SQL editor with syntax highlighting and multiple tabs
- 📊 Result grid with pagination, sort, filter
- 🕘 Query history
- 🔐 Encrypted credential storage via OS keychain

## Architecture

Cargo workspace, driver-per-database:

```
narwhal-core              # traits, types, errors
narwhal-config            # TOML config + keyring
narwhal-driver-postgres   # PostgreSQL impl
narwhal-driver-sqlite     # SQLite impl
narwhal-vim               # Vim mode state machine
narwhal-tui               # Ratatui UI layer
narwhal-app               # event loop, state mgmt
narwhal                   # binary
```

## Build

```bash
cargo build --release
./target/release/narwhal
```

## License

Dual-licensed under MIT OR Apache-2.0.
