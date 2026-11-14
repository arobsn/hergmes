//! The follower: one loop that reads `/info` once per tick and serves it to the block consumer
//! and then to the mempool poller.
//!
//! One read serves both, and the order matters: the engine applies the blocks up to the tip
//! the read shows before the mempool is fetched at that tip, so a snapshot arrives at a tip
//! the engine already has.
//!
//! The mempool is polled only once the engine is at the node's tip. A snapshot taken while it
//! is behind would be held or ignored by the engine, but counted as taken by the poller, which
//! would then wait for its refresh period before fetching again at the tip the engine
//! reaches.

use std::time::Duration;

use tokio::time::{Instant, MissedTickBehavior, sleep};
use tracing::{info, warn};

use crate::{
    adapters::{
        AdapterError, EngineGone, EngineHandle, backoff, best_full_block,
        blocks::{BlockConsumer, Sync},
        mempool::{self, MempoolPoller, Poll},
    },
    clients::{NodeError, NodeSource},
    engine::Event,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Config {
    /// How often `/info` is read.
    pub interval: Duration,
    pub mempool: mempool::Config,
}

impl Default for Config {
    fn default() -> Self {
        Self { interval: Duration::from_secs(1), mempool: mempool::Config::default() }
    }
}

/// What one tick did.
#[derive(Debug)]
pub struct Tick {
    pub blocks: Result<Sync, NodeError>,
    /// `None` when the engine is not at the node's tip: the mempool was not polled.
    pub mempool: Option<Result<Poll, NodeError>>,
}

/// Follows the node's tip and mempool.
pub struct Follower<S> {
    source: S,
    blocks: BlockConsumer<S>,
    mempool: MempoolPoller<S>,
    config: Config,
}

impl<S: NodeSource + Clone> Follower<S> {
    pub fn new(source: S, config: Config) -> Self {
        Self {
            blocks: BlockConsumer::new(source.clone()),
            mempool: MempoolPoller::new(source.clone(), config.mempool),
            source,
            config,
        }
    }

    /// One tick at `now`: read `/info`, bring the engine to its tip, then, if it got there,
    /// poll the mempool and send the snapshot taken. `Err` only if `/info` itself failed or
    /// the engine is gone.
    pub async fn tick(
        &mut self,
        engine: &impl EngineHandle,
        now: Instant,
    ) -> Result<Tick, AdapterError> {
        let info = self.source.info().await?;
        let blocks = match best_full_block(&info) {
            Some(tip) => match self.blocks.sync(engine, tip).await {
                Ok(sync) => Ok(sync),
                Err(error) => Err(error.into_node()?),
            },
            None => Ok(Sync::Idle),
        };
        if !matches!(blocks, Ok(Sync::Idle | Sync::Synced { .. })) {
            return Ok(Tick { blocks, mempool: None });
        }
        let mempool = self.mempool.poll(info, now).await;
        if let Ok(Poll::Snapshot { txs, observed_tip }) = &mempool {
            let event = Event::MempoolSnapshot { txs: txs.clone(), observed_tip: *observed_tip };
            // The engine never refuses a snapshot: one off its tip is held or ignored.
            let _ = engine.apply(event).await?;
        }
        Ok(Tick { blocks, mempool: Some(mempool) })
    }

    /// Ticks every interval until the engine is gone. A failed call is logged, and a `503`
    /// waits for its `Retry-After` before the next tick. A refused event is logged by the
    /// engine task; the next tick syncs again from the engine's chain.
    pub async fn run(mut self, engine: &impl EngineHandle) -> EngineGone {
        let mut ticker = tokio::time::interval(self.config.interval);
        ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
        loop {
            ticker.tick().await;
            let wait = match self.tick(engine, Instant::now()).await {
                Ok(tick) => {
                    if let Some(Err(error)) = &tick.mempool {
                        warn!(%error, "Mempool poll failed");
                    }
                    tick.blocks.err()
                }
                Err(AdapterError::Node(error)) => Some(error),
                Err(AdapterError::EngineGone(gone)) => return gone,
            };
            if let Some(error) = wait {
                let wait = backoff(&error, self.config.interval);
                match error {
                    NodeError::Unavailable { .. } => {
                        info!(?wait, "Node unavailable (503), backing off");
                    }
                    error => warn!(%error, "Block sync failed"),
                }
                sleep(wait).await;
            }
        }
    }
}

#[cfg(test)]
#[path = "follower_tests.rs"]
mod tests;
