//! The core: a pure state machine with no I/O and no clock.
//!
//! Owns the per-key unspent id sets, the block delta ring buffer and the
//! mempool overlays. Driven by `apply(event) -> Vec<Output>`.

mod block;
mod key;
mod mempool;
mod message;
pub mod overlay;

pub use block::{BlockDelta, BlockRef, BoxKey, KeyDelta, Spend, StoredBox, Totals};
pub use key::TrackingKey;
pub use mempool::Mempool;
pub use message::{Event, Output, PageError, PendingTx, Query, Reply, ResyncReason};
