# narwhal-vim

Modal keystroke processor (vim semantics).

A pure state machine: consumes logical `Key` events and emits `Action`s describing buffer mutations. Terminal-backend agnostic.

Part of the [narwhal](../../README.md) workspace. See [`docs/ARCHITECTURE.md`](../../docs/ARCHITECTURE.md) for the layered crate map.
