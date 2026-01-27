# narwhal-driver-sqlite

SQLite driver backed by `rusqlite`.

Synchronous calls dispatched onto `tokio::task::spawn_blocking`, with the connection protected by a `tokio::sync::Mutex`.

Part of the [narwhal](../../README.md) workspace. See [`docs/ARCHITECTURE.md`](../../docs/ARCHITECTURE.md) for the layered crate map.
