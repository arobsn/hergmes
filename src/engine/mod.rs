//! The core: a pure state machine with no I/O and no clock.
//!
//! Owns the per-address unspent id sets, the block delta ring buffer and the
//! mempool overlays. Driven by `apply(event) -> Vec<Output>`. See DESIGN.md.
