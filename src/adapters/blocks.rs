//! The block consumer: brings the engine to the node's best full block, through reverts and
//! complete indexed blocks.
//!
//! It keeps no chain of its own. Each sync starts from the engine's chain (`Query::Chain`),
//! so the two can't drift apart, and an `EngineError` needs no repair beyond the next sync.

use tracing::{debug, info, warn};

use crate::{
    adapters::{AdapterError, EngineGone, EngineHandle, engine_chain, engine_tip},
    clients::{NodeError, NodeSource},
    engine::{BlockRef, ChainLink, EngineError, Event},
    types::ergo::IndexedBlock,
};

/// The outcome of one sync.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Sync {
    /// The engine was already at the node's tip.
    Idle,
    /// The indexer has not reached the node's tip, so its block can't be fetched whole yet.
    WaitingForIndexer { indexed_height: u32, tip_height: u32 },
    /// A block of the walk was not served although the indexer had reached its height: the
    /// node's tip moved again. Nothing was sent; the next sync starts from the new tip.
    TipMoved,
    /// The engine reached the node's tip.
    Synced { reverted: usize, applied: usize, reset: bool },
    /// The engine refused an event. Not expected, since the events are built from the
    /// engine's own chain; the next sync starts from that chain again.
    Refused(EngineError),
}

/// Where the walk back from the node's tip met the engine's chain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ForkPoint {
    /// The engine has no tip: the node's tip is applied alone.
    Start,
    /// A block of the engine's chain, by index.
    At(usize),
    /// The parent of the engine's oldest block. Reverting every block reaches it, and the
    /// engine then accepts its child.
    BelowChain,
    /// Below that: deeper than the engine can revert. Every key resyncs.
    BeyondWindow,
}

/// Follows the node's best full block.
pub struct BlockConsumer<S> {
    source: S,
}

impl<S: NodeSource> BlockConsumer<S> {
    pub fn new(source: S) -> Self {
        Self { source }
    }

    /// Brings the engine to `tip`, the best full block from an `/info` read.
    ///
    /// If `tip` is in the engine's chain, the blocks above it are reverted and nothing is
    /// fetched. Otherwise the consumer waits until `indexedHeight` reaches `tip`, then walks
    /// back from it through indexed blocks (`header.parentId`) to the engine's chain, and
    /// sends the reverts above the fork point and the walked blocks, oldest first. The walk
    /// is complete before anything is sent, so the engine never gets part of a reorg.
    ///
    /// A block the indexer does not serve, at or below `indexedHeight`, means the tip moved
    /// again (the block was orphaned), not an error. A `503` while the indexer recovers comes
    /// back as `NodeError::Unavailable`, with nothing sent.
    pub async fn sync(
        &self,
        engine: &impl EngineHandle,
        tip: BlockRef,
    ) -> Result<Sync, AdapterError> {
        if engine_tip(engine).await? == Some(tip) {
            return Ok(Sync::Idle);
        }
        let chain = engine_chain(engine).await?;

        // The node went back to a block the engine has: revert only.
        if let Some(at) = chain.iter().position(|l| l.block == tip) {
            let reverts = chain[at + 1..].iter().rev().map(|l| l.block).collect();
            return Ok(send(engine, reverts, Vec::new(), false).await?);
        }

        let indexed = self.source.indexed_height().await?;
        if indexed.indexed_height < tip.height {
            debug!(indexed = indexed.indexed_height, tip = tip.height, "Waiting for the indexer");
            return Ok(Sync::WaitingForIndexer {
                indexed_height: indexed.indexed_height,
                tip_height: tip.height,
            });
        }

        let Some((mut walked, fork)) = self.walk(&chain, tip).await? else {
            debug!(?tip, "Block walk abandoned: the tip moved again");
            return Ok(Sync::TipMoved);
        };
        let (reverts, reset) = match fork {
            ForkPoint::Start => (Vec::new(), false),
            ForkPoint::At(at) => (chain[at + 1..].iter().rev().map(|l| l.block).collect(), false),
            ForkPoint::BelowChain => (chain.iter().rev().map(|l| l.block).collect(), false),
            ForkPoint::BeyondWindow => {
                // One more revert than the chain holds: the engine resyncs every key and
                // drops its tip, then takes the node's tip as a new start.
                let mut reverts: Vec<BlockRef> = chain.iter().rev().map(|l| l.block).collect();
                reverts.push(chain[0].parent());
                walked.truncate(1);
                warn!(
                    ?tip,
                    oldest = ?chain[0].block,
                    "Fork deeper than the window: every key resyncs"
                );
                (reverts, true)
            }
        };
        walked.reverse();
        Ok(send(engine, reverts, walked, reset).await?)
    }

    /// The indexed blocks from `tip` down to the engine's chain, newest first, and where the
    /// walk met it. `None` if a block was not served.
    async fn walk(
        &self,
        chain: &[ChainLink],
        tip: BlockRef,
    ) -> Result<Option<(Vec<IndexedBlock>, ForkPoint)>, NodeError> {
        let mut walked = Vec::new();
        let mut cursor = tip;
        loop {
            let Some(block) = self.source.indexed_block(&cursor.header_id).await? else {
                return Ok(None);
            };
            if BlockRef::from(&block.header) != cursor {
                return Ok(None);
            }
            let parent = BlockRef {
                height: cursor.height.saturating_sub(1),
                header_id: block.header.parent_id,
            };
            walked.push(block);
            let Some(oldest) = chain.first() else {
                return Ok(Some((walked, ForkPoint::Start)));
            };
            if let Some(at) = chain.iter().position(|l| l.block == parent) {
                return Ok(Some((walked, ForkPoint::At(at))));
            }
            if parent == oldest.parent() {
                return Ok(Some((walked, ForkPoint::BelowChain)));
            }
            if parent.height < oldest.block.height {
                return Ok(Some((walked, ForkPoint::BeyondWindow)));
            }
            cursor = parent;
        }
    }
}

/// Sends the reverts, newest first, then the blocks, oldest first.
async fn send(
    engine: &impl EngineHandle,
    reverts: Vec<BlockRef>,
    blocks: Vec<IndexedBlock>,
    reset: bool,
) -> Result<Sync, EngineGone> {
    let (reverted, applied) = (reverts.len(), blocks.len());
    let from = reverts.first().copied();
    let to = blocks.last().map(|b| BlockRef::from(&b.header));
    for block in reverts {
        if let Err(error) = engine.apply(Event::BlockReverted(block)).await? {
            return Ok(Sync::Refused(error));
        }
    }
    for block in blocks {
        if let Err(error) = engine.apply(Event::BlockApplied(block)).await? {
            return Ok(Sync::Refused(error));
        }
    }
    if reverted > 0 {
        info!(reverted, applied, ?from, ?to, "Reorg");
    } else {
        debug!(applied, ?to, "Blocks applied");
    }
    Ok(Sync::Synced { reverted, applied, reset })
}

#[cfg(test)]
#[path = "blocks_tests.rs"]
mod tests;
