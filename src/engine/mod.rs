//! The core: a pure state machine with no I/O and no clock.
//!
//! Owns the per-key unspent id sets, the block delta ring buffer and the
//! mempool overlays. Driven by [`Engine::apply`] and [`Engine::query`].

mod block;
mod key;
mod mempool;
mod message;
pub mod overlay;
mod state;

pub use block::{
    BlockDelta, BlockRef, BoxKey, ChainLink, KeyDelta, KeyId, Spend, StoredBox, Totals,
};
pub use key::TrackingKey;
pub use mempool::Mempool;
pub use message::{
    Balance, EngineError, Event, Output, PageError, PendingTx, Query, Reply, ResyncReason,
};
pub use state::{Config, Engine};
