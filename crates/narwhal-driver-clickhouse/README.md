# narwhal-driver-clickhouse

ClickHouse driver using the native HTTP interface.

Streams `TabSeparatedWithNamesAndTypes` results through `reqwest::Response::bytes_stream()`.

Part of the [narwhal](../../README.md) workspace. See [`docs/ARCHITECTURE.md`](../../docs/ARCHITECTURE.md) for the layered crate map.
