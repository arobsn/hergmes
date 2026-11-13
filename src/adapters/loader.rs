//! The first-load loader: a key's whole unspent set from the indexer, pinned to one block,
//! sent to the engine as `KeyLoaded`.
//!
//! The indexer can't pin a read to a block, so the loader pins it from outside: the node's
//! tip is read before and after the pages, and the indexer must be at that tip
//! (DESIGN.md, "How the hybrid works", First load).

use std::time::Duration;

use tokio::time::sleep;
use tracing::{debug, info};

use crate::{
    adapters::{EngineHandle, backoff, best_full_block, engine_tip},
    clients::{NodeError, NodeSource, Page, source::MAX_PAGE_LIMIT},
    engine::{BlockRef, Event, Output, TrackingKey},
    types::ergo::{BoxId, IndexedBox},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Config {
    /// Boxes per page, at most [`MAX_PAGE_LIMIT`] (the node rejects more).
    pub page_limit: u32,
    /// Attempts before [`Loader::load`] gives up. Each one that reaches the indexer is a full
    /// reload, so this stays small.
    pub attempts: u32,
    /// The wait between attempts: about one tick of the follower, which is what an engine
    /// behind the node needs to catch up.
    pub retry: Duration,
}

impl Default for Config {
    fn default() -> Self {
        Self { page_limit: MAX_PAGE_LIMIT, attempts: 10, retry: Duration::from_secs(1) }
    }
}

/// The outcome of one attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Attempt {
    /// The engine accepted the load and tracks the key.
    Tracked { as_of: BlockRef, boxes: usize },
    /// The node has no full block yet.
    NoTip,
    /// The engine is not at the node's tip, so a load there would be rejected. Nothing was
    /// loaded; the block consumer catches up first.
    EngineBehind { engine: Option<BlockRef>, node: BlockRef },
    /// The indexer is not at the node's tip.
    IndexerBehind { indexed_height: u32, tip_height: u32 },
    /// The node's tip moved during the load. The boxes were dropped, never sent mixed.
    TipMoved,
    /// The engine moved between its tip read and the load (`LoadRejected`).
    Rejected,
}

#[derive(Debug, thiserror::Error)]
pub enum LoadError {
    #[error(transparent)]
    Node(#[from] NodeError),
    /// The indexer returned a box the key does not match. A node bug; the key is not tracked.
    #[error("the indexer returned box {box_id} that does not match the key")]
    Mismatch { box_id: BoxId },
    #[error("no load settled after {attempts} attempts, the last one {last:?}")]
    GaveUp { attempts: u32, last: Attempt },
}

pub struct Loader<S> {
    source: S,
    config: Config,
}

impl<S: NodeSource> Loader<S> {
    pub fn new(source: S, config: Config) -> Self {
        assert!(config.page_limit >= 1 && config.page_limit <= MAX_PAGE_LIMIT);
        Self { source, config }
    }

    /// Loads `key` and sends it to the engine, retrying until an attempt is accepted or
    /// `attempts` have been made. A `503` waits for its `Retry-After`; other node errors end
    /// the load.
    pub async fn load(
        &self,
        engine: &impl EngineHandle,
        key: &TrackingKey,
    ) -> Result<(BlockRef, usize), LoadError> {
        let mut last = Attempt::NoTip;
        for attempt in 1..=self.config.attempts {
            let wait = match self.try_load(engine, key).await {
                Ok(Attempt::Tracked { as_of, boxes }) => return Ok((as_of, boxes)),
                Ok(outcome) => {
                    if outcome == Attempt::Rejected {
                        info!(?key, attempt, "Load rejected by the engine, retrying");
                    } else {
                        debug!(?key, attempt, ?outcome, "Load not settled, retrying");
                    }
                    last = outcome;
                    self.config.retry
                }
                Err(LoadError::Node(error @ NodeError::Unavailable { .. })) => {
                    let wait = backoff(&error, self.config.retry);
                    info!(?key, ?wait, "Indexer unavailable (503) during a load, backing off");
                    wait
                }
                Err(error) => return Err(error),
            };
            if attempt < self.config.attempts {
                sleep(wait).await;
            }
        }
        Err(LoadError::GaveUp { attempts: self.config.attempts, last })
    }

    /// One attempt: read the tip, check the engine and the indexer are at it, read every
    /// page oldest first, read the tip again, check every box against the key, and send
    /// `KeyLoaded` pinned to that tip.
    pub async fn try_load(
        &self,
        engine: &impl EngineHandle,
        key: &TrackingKey,
    ) -> Result<Attempt, LoadError> {
        let Some(tip) = best_full_block(&self.source.info().await?) else {
            return Ok(Attempt::NoTip);
        };
        let engine_at = engine_tip(engine).await;
        if engine_at != Some(tip) {
            return Ok(Attempt::EngineBehind { engine: engine_at, node: tip });
        }
        let indexed = self.source.indexed_height().await?;
        if indexed.indexed_height != tip.height {
            return Ok(Attempt::IndexerBehind {
                indexed_height: indexed.indexed_height,
                tip_height: tip.height,
            });
        }

        let boxes = self.pages(key).await?;
        if best_full_block(&self.source.info().await?) != Some(tip) {
            debug!(?key, ?tip, "Load dropped: the tip moved during it");
            return Ok(Attempt::TipMoved);
        }
        if let Some(b) = boxes.iter().find(|b| !key.matches(&b.utxo)) {
            return Err(LoadError::Mismatch { box_id: b.utxo.id });
        }

        let count = boxes.len();
        let event = Event::KeyLoaded { key: key.clone(), as_of: tip, boxes };
        // `KeyLoaded` is never an `EngineError`: an off-tip load answers `LoadRejected`.
        let outputs = engine.apply(event).await.unwrap_or_default();
        let rejected = outputs
            .iter()
            .any(|o| matches!(o, Output::LoadRejected { key: k, .. } if k == key));
        Ok(if rejected { Attempt::Rejected } else { Attempt::Tracked { as_of: tip, boxes: count } })
    }

    /// Every page of the key's unspent boxes, oldest first, until a short page.
    async fn pages(&self, key: &TrackingKey) -> Result<Vec<IndexedBox>, NodeError> {
        let limit = self.config.page_limit;
        let mut boxes = Vec::new();
        loop {
            let page = Page { offset: boxes.len() as u32, limit };
            let mut read = match key {
                TrackingKey::Tree(tree) => self.source.unspent_by_ergo_tree(tree, page).await?,
                TrackingKey::Token(id) => self.source.unspent_by_token(id, page).await?,
            };
            let full = read.len() as u32 >= limit;
            boxes.append(&mut read);
            if !full {
                return Ok(boxes);
            }
        }
    }
}

#[cfg(test)]
#[path = "loader_tests.rs"]
mod tests;
