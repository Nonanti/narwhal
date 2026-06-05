//! Memory-limit primitives.
//!
//! Currently a thin wrapper over [`wasmtime::StoreLimitsBuilder`].
//! Lives in its own module so the host-side glue can stay focused
//! on WIT bindings and the future addition of table/instance limits
//! lands in one well-known spot.

use wasmtime::{StoreLimits, StoreLimitsBuilder};

/// Build the wasmtime store limits enforced on every plugin's
/// `Store`. The memory ceiling is hard — wasmtime traps the guest
/// once the linear memory growth event would push past it.
///
/// Table size is left unbounded by design: components produced by
/// `wit-bindgen` use a single small table that the engine sizes at
/// load; adding a knob here would be cargo-culting without a real
/// abuse vector. Should the threat model change, this is the one
/// function to extend.
#[must_use]
pub fn build_store_limits(memory_bytes: usize) -> StoreLimits {
    StoreLimitsBuilder::new().memory_size(memory_bytes).build()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_does_not_panic_on_zero() {
        // Wasmtime accepts zero — useful for tests that want to
        // simulate a memory-starved component.
        let _ = build_store_limits(0);
    }

    #[test]
    fn build_returns_a_limits_value() {
        // Smoke-test: the StoreLimits type intentionally has no
        // public accessors (the engine consumes it directly), so we
        // only verify the call shape.
        let _ = build_store_limits(64 * 1024 * 1024);
    }
}
