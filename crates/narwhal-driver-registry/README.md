# narwhal-driver-registry

Driver registry with feature-gated bundled drivers.

Hosts that need to address a `DatabaseDriver` by name (`narwhal-app`, `narwhal-mcp`, headless CLI) all consume this crate. Concrete driver crates are pulled in by cargo features: `driver-postgres`, `driver-sqlite`, `driver-mysql`, `driver-duckdb`, `driver-clickhouse`, `all-drivers`.

Part of the [narwhal](../../README.md) workspace. See [`docs/ARCHITECTURE.md`](../../docs/ARCHITECTURE.md) for the layered crate map.
