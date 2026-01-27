# narwhal-history

Append-only JSONL history journal.

Each executed statement is written as a single JSON object on its own line so concurrent writers do not corrupt each other. Streaming reads never materialise the whole file.

Part of the [narwhal](../../README.md) workspace. See [`docs/ARCHITECTURE.md`](../../docs/ARCHITECTURE.md) for the layered crate map.
