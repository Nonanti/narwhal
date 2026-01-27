# narwhal-driver-postgres

PostgreSQL driver backed by `tokio-postgres`.

Honors `ssl_mode` (`disable`/`prefer`/`require`/`verify-ca`/`verify-full`). Streams large result sets through `Connection::stream`.

Part of the [narwhal](../../README.md) workspace. See [`docs/ARCHITECTURE.md`](../../docs/ARCHITECTURE.md) for the layered crate map.
