use std::sync::Arc;

use serde::Serialize;

use crate::{
    engine::{BlockRef, BoxKey, Spend, Totals, TrackingKey, overlay::PendingTotals},
    types::ergo::{BoxId, HeaderId, IndexedBlock, IndexedBox, TxId, UTxO, UnconfirmedTransaction},
};

/// An input to the engine, sent by the adapters.
#[derive(Debug, Clone)]
pub enum Event {
    /// A block extending the current tip, with full input boxes.
    BlockApplied(IndexedBlock),
    /// The current tip was orphaned. The engine undoes it from its ring buffer.
    BlockReverted(BlockRef),
    /// The whole mempool, and the tip read alongside it.
    MempoolSnapshot { txs: Arc<[UnconfirmedTransaction]>, observed_tip: BlockRef },
    /// A key's first load. Accepted only if `as_of` is the current tip; it then starts tracking.
    KeyLoaded { key: TrackingKey, as_of: BlockRef, boxes: Vec<IndexedBox> },
    /// A client subscribed to a key.
    Subscribed(TrackingKey),
    /// A client unsubscribed from a key or disconnected. A key with no subscribers is
    /// untracked once it has been idle for the grace period, counted in blocks.
    Unsubscribed(TrackingKey),
}

/// A read of engine state, answered in order with the events.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Query {
    /// Up to `limit` box keys of `key` as of block `at`, after the cursor `after`.
    Page {
        key: TrackingKey,
        at: BlockRef,
        after: Option<BoxKey>,
        limit: usize,
    },
    /// A key's overlay, tracked or not.
    Overlay(TrackingKey),
    /// A key's pending totals, tracked or not: the pending side of a balance.
    Pending(TrackingKey),
    Totals(TrackingKey),
    Tip,
    MempoolSnapshot,
    /// The messages that bring a client from `from` to the tip: replayed blocks or
    /// rollbacks then the overlay, or a resync.
    CatchUp {
        key: TrackingKey,
        from: BlockRef,
    },
}

/// The answer to a [`Query`], in the matching variant. Where a reply is an `Option`, `None`
/// means the key is untracked; overlays and pending totals are answered for any key.
#[derive(Debug, Clone)]
pub enum Reply {
    Page(Result<Vec<BoxKey>, PageError>),
    Overlay(Vec<PendingTx>),
    Pending(PendingTotals),
    Totals(Option<Totals>),
    Tip(Option<BlockRef>),
    MempoolSnapshot(Option<(Arc<[UnconfirmedTransaction]>, BlockRef)>),
    CatchUp(Vec<Output>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum PageError {
    #[error("key is not tracked")]
    NotTracked,
    #[error("block is no longer on the main chain or outside the retention window")]
    CursorExpired,
}

/// A message for the subscribers of one key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Output {
    Block {
        key: TrackingKey,
        block: BlockRef,
        parent_id: HeaderId,
        created: Vec<Arc<UTxO>>,
        spent: Vec<Spend>,
    },
    /// The inverse of an orphaned block's delta.
    Rollback {
        key: TrackingKey,
        block: BlockRef,
        created: Vec<Arc<UTxO>>,
        spent: Vec<Spend>,
    },
    /// The key's whole pending state, replacing the previous one.
    Mempool {
        key: TrackingKey,
        tip_height: u32,
        pending: Vec<PendingTx>,
    },
    ResyncRequired {
        key: TrackingKey,
        reason: ResyncReason,
    },
    /// A first load pinned to a block other than the tip; the loader retries.
    LoadRejected {
        key: TrackingKey,
        as_of: BlockRef,
    },
}

/// A pending transaction as it affects one key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingTx {
    pub tx_id: TxId,
    pub created: Vec<Arc<UTxO>>,
    pub spent: Vec<BoxId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ResyncReason {
    OutsideWindow,
    ForkBeyondWindow,
    NotTracked,
    SlowConsumer,
}
