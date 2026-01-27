# narwhal-pool

Async connection pool keyed by `ConnectionConfig` + credential.

Lazy connection creation up to a configurable ceiling, health checks on hand-out, recycling on drop. Driver-agnostic.

Part of the [narwhal](../../README.md) workspace. See [`docs/ARCHITECTURE.md`](../../docs/ARCHITECTURE.md) for the layered crate map.
