# narwhal-driver-duckdb

DuckDB driver backed by the `duckdb` crate.

Embedded OLAP engine. Rich type lattice (huge ints, intervals, structs, maps, unions) mapped lossily in the internal `types` module. Supports query cancellation through `InterruptHandle`.

Part of the [narwhal](../../README.md) workspace. See [`docs/ARCHITECTURE.md`](../../docs/ARCHITECTURE.md) for the layered crate map.
