//! The mempool poller: whole-pool snapshots, stamped with the tip they were read at.
//!
//! The pool is fetched whole and nothing is kept between reads (DESIGN.md, "How the hybrid
//! works"): how the node resolves a tx's inputs is its verdict at one moment, so a tx cached
//! from an earlier read could carry inputs a block has since spent.

use std::{sync::Arc, time::Duration};

use tokio::{
    sync::mpsc,
    time::{Instant, MissedTickBehavior},
};
use tracing::{debug, info, warn};

use crate::{
    clients::{NodeError, NodeSource, node::InfoResponse},
    engine::{BlockRef, Event},
    types::ergo::UnconfirmedTransaction,
};

/// How often the poller looks at the node, and how long it trusts an unchanged pool.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Config {
    /// How often `/info` is read. A read that shows a change fetches the pool.
    pub interval: Duration,
    /// How long after a snapshot the pool is fetched again even if `/info` shows no change.
    /// `lastMemPoolUpdateTime` is a hint: a fetch right after it moves can still return the
    /// previous pool, and nothing moves it again. Each snapshot makes the engine recompute
    /// every tracked overlay, so it is kept well above `interval`.
    pub refresh: Duration,
}

impl Default for Config {
    fn default() -> Self {
        Self { interval: Duration::from_secs(1), refresh: Duration::from_secs(10) }
    }
}

/// How many times one poll fetches the pool while the tip keeps moving under it. Bounded so
/// a node switching tips on every read is not hammered; the next poll tries again.
pub const ATTEMPTS: u32 = 3;

/// The outcome of one poll.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Poll {
    /// The pool, read between two `/info` reads that showed the same tip.
    Snapshot { txs: Arc<[UnconfirmedTransaction]>, observed_tip: BlockRef },
    /// Neither the tip nor `lastMemPoolUpdateTime` changed since the last snapshot, and the
    /// refresh period has not passed.
    Unchanged,
    /// The node has no full block yet, so a snapshot could not be stamped.
    NoTip,
    /// The tip moved during every fetch. Nothing is remembered, so the next poll fetches.
    TipMoved,
}

/// What the last snapshot was taken against.
#[derive(Debug, Clone, Copy)]
struct Taken {
    /// `lastMemPoolUpdateTime` from the read before the fetch. A change that lands during the
    /// fetch then still differs from it, and the next poll fetches again.
    update: u64,
    tip: BlockRef,
    at: Instant,
}

/// Polls the node's mempool and stamps each snapshot with `observed_tip`.
pub struct MempoolPoller<S> {
    source: S,
    config: Config,
    last: Option<Taken>,
}

/// The best full block `/info` reports, if the node has one.
fn tip_of(info: &InfoResponse) -> Option<BlockRef> {
    Some(BlockRef { height: info.full_height?, header_id: info.best_full_header_id? })
}

impl<S: NodeSource> MempoolPoller<S> {
    pub fn new(source: S, config: Config) -> Self {
        Self { source, config, last: None }
    }

    /// One poll at `now`: read `/info`, and fetch the pool if anything changed.
    ///
    /// The pool is fetched when `lastMemPoolUpdateTime` or the tip differs from the last
    /// snapshot's, or the refresh period has passed. The time is compared by equality, since
    /// it is the node's wall clock. The tip is compared too because the node updates them
    /// from different events (`ErgoStatsCollector`: the tip in `onHistoryChanged`, on
    /// `ChangedHistory`; the time in `onMempoolChanged`, on `ChangedMempool`), so one read can
    /// show either change without the other.
    ///
    /// A snapshot is kept only if `/info` shows the same tip before and after the fetch;
    /// otherwise it is discarded and fetched again, up to [`ATTEMPTS`] times. A failed call
    /// or a discarded snapshot leaves nothing remembered, so the next poll fetches.
    pub async fn poll(&mut self, now: Instant) -> Result<Poll, NodeError> {
        let mut before = self.source.info().await?;
        let Some(mut tip) = tip_of(&before) else {
            debug!("Mempool not polled: the node has no full block yet");
            return Ok(Poll::NoTip);
        };
        if let Some(last) = self.last
            && last.update == before.last_mempool_update
            && last.tip == tip
            && now.duration_since(last.at) < self.config.refresh
        {
            return Ok(Poll::Unchanged);
        }

        for attempt in 1..=ATTEMPTS {
            let txs = self.source.mempool().await?;
            let after = self.source.info().await?;
            let moved_to = tip_of(&after);
            if moved_to == Some(tip) {
                self.last = Some(Taken { update: before.last_mempool_update, tip, at: now });
                debug!(txs = txs.len(), height = tip.height, "Mempool snapshot taken");
                return Ok(Poll::Snapshot { txs: txs.into(), observed_tip: tip });
            }
            info!(
                ?tip,
                ?moved_to,
                attempt,
                "Mempool snapshot discarded: the tip moved during the fetch"
            );
            let Some(next) = moved_to else {
                return Ok(Poll::NoTip);
            };
            before = after;
            tip = next;
        }
        Ok(Poll::TipMoved)
    }

    /// Polls every interval and sends each snapshot to the engine, until its inbox closes.
    /// A failed poll is logged and retried at the next interval.
    pub async fn run(mut self, events: mpsc::Sender<Event>) {
        let mut ticker = tokio::time::interval(self.config.interval);
        ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
        loop {
            ticker.tick().await;
            match self.poll(Instant::now()).await {
                Ok(Poll::Snapshot { txs, observed_tip }) => {
                    let event = Event::MempoolSnapshot { txs, observed_tip };
                    if events.send(event).await.is_err() {
                        info!("Mempool poller stopped: the engine's inbox is closed");
                        return;
                    }
                }
                Ok(Poll::Unchanged | Poll::NoTip | Poll::TipMoved) => {}
                Err(error) => warn!(%error, "Mempool poll failed"),
            }
        }
    }
}

#[cfg(test)]
#[path = "mempool_tests.rs"]
mod tests;
