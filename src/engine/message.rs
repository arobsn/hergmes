use std::sync::Arc;

use serde::Serialize;

use crate::{
    engine::{BlockRef, BoxKey, ChainLink, Spend, Totals, TrackingKey, overlay::PendingDelta},
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
    /// A client subscribed to a key. A key that is not tracked answers `ResyncRequired`: the
    /// transport loads it first.
    Subscribed(TrackingKey),
    /// A client unsubscribed from a key or disconnected. A key with no subscribers is
    /// untracked once it has been idle for the grace period, counted in blocks.
    Unsubscribed(TrackingKey),
}

/// A read of engine state, answered in order with the events. Reads of a tracked key count
/// as activity for the grace period.
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
    /// A key's pending delta, tracked or not: the signed pending side of a balance.
    Pending(TrackingKey),
    /// A tracked key's balance at the tip, as the stream sends it.
    Balance(TrackingKey),
    Tip,
    /// The blocks the engine can revert, oldest first, ending at the tip. The block consumer
    /// finds the fork point in it.
    Chain,
    /// Full boxes the ring buffer holds, for the box fetcher. Ids it does not hold are left
    /// out.
    Boxes(Vec<BoxId>),
    MempoolSnapshot,
    /// The messages that bring a client from `from` to the tip: replayed blocks or
    /// rollbacks, then the overlay and the balance, or a resync. Without `from` (a
    /// balance-only subscription), only the balance.
    CatchUp {
        key: TrackingKey,
        from: Option<BlockRef>,
    },
}

/// The answer to a [`Query`], in the matching variant. Where a reply is an `Option`, `None`
/// means the key is untracked or there is no tip; overlays and pending deltas are answered
/// for any key.
#[derive(Debug, Clone)]
pub enum Reply {
    Page(Result<Vec<BoxKey>, PageError>),
    Overlay(Vec<PendingTx>),
    Pending(PendingDelta),
    Balance(Option<Balance>),
    Tip(Option<BlockRef>),
    Chain(Vec<ChainLink>),
    Boxes(Vec<Arc<UTxO>>),
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

/// An event the engine cannot apply: the block consumer's view of the chain is not the
/// engine's. Nothing changed; the consumer resynchronizes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum EngineError {
    #[error("block {block:?} does not extend the tip {tip:?}")]
    NotChild { block: BlockRef, tip: Option<BlockRef> },
    #[error("block {block:?} is not the tip {tip:?}")]
    NotTip { block: BlockRef, tip: Option<BlockRef> },
}

/// A key's balance at a tip: its running totals, and the signed pending delta of its overlay.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Balance {
    pub as_of: BlockRef,
    pub confirmed: Totals,
    pub pending: PendingDelta,
}

/// A message for the subscribers of one key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Output {
    /// A block applied on top of `parent_id`. Every tracked key gets one per block, empty or
    /// not, so its subscribers can check the parent chain.
    Block {
        key: TrackingKey,
        block: BlockRef,
        parent_id: HeaderId,
        created: Vec<Arc<UTxO>>,
        spent: Vec<Spend>,
    },
    /// The inverse of an orphaned block's delta: `created` are the boxes it had spent, and
    /// `spent` the boxes it had created (with the tx that created them). Afterwards the
    /// subscriber is at `parent_id`.
    Rollback {
        key: TrackingKey,
        block: BlockRef,
        parent_id: HeaderId,
        created: Vec<Arc<UTxO>>,
        spent: Vec<Spend>,
    },
    /// The key's whole pending state, replacing the previous one.
    Mempool { key: TrackingKey, tip_height: u32, pending: Vec<PendingTx> },
    /// The key's whole balance, replacing the previous one. Sent when its value changes.
    Balance { key: TrackingKey, balance: Balance },
    /// The subscription ended: the client redoes its snapshot, or for a balance-only one,
    /// subscribes again.
    ResyncRequired { key: TrackingKey, reason: ResyncReason },
    /// A first load pinned to a block other than the tip; the loader retries.
    LoadRejected { key: TrackingKey, as_of: BlockRef },
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
    /// The client's block is below the retention window.
    OutsideWindow,
    /// The client's block is not on the main chain, and the engine cannot roll it back: the
    /// fork is deeper than the window, or the block was never seen. Also sent when a block the
    /// key was loaded at is orphaned.
    ForkBeyondWindow,
    /// The key is not tracked, or was not tracked over the client's blocks.
    NotTracked,
    SlowConsumer,
}
