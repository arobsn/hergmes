//! Adapters: all I/O against the node.
//!
//! Block consumer, mempool poller, first-load loader, token checker, box fetcher and balance
//! reader. They fetch, stamp with the observed tip and send events to the engine. Each one
//! has a core that never sleeps (one sync, one poll, one load attempt), so tests drive it
//! against `SimChain` step by step; waiting and back-off live in thin loops around it.

use std::{
    future::Future,
    sync::{Mutex, PoisonError},
    time::Duration,
};

use crate::{
    clients::{NodeError, node::InfoResponse},
    engine::{BlockRef, ChainLink, Engine, EngineError, Event, Output, Query, Reply},
};

pub mod balance;
pub mod blocks;
pub mod boxes;
pub mod follower;
pub mod loader;
pub mod mempool;
pub mod tokens;

pub use balance::BalanceReader;
pub use blocks::BlockConsumer;
pub use boxes::BoxFetcher;
pub use follower::Follower;
pub use loader::Loader;
pub use mempool::MempoolPoller;
pub use tokens::TokenChecker;

/// How adapters reach the engine: an event with what it produced, or a query with its reply.
///
/// The block consumer needs the answer to an event (an `EngineError` makes it start again
/// from the engine's chain), and so does the loader (`LoadRejected` makes it retry), so
/// events are not fire-and-forget. Step 7's engine task implements this over its inbox.
pub trait EngineHandle: Send + Sync {
    fn apply(&self, event: Event) -> impl Future<Output = Result<Vec<Output>, EngineError>> + Send;

    fn query(&self, query: Query) -> impl Future<Output = Reply> + Send;
}

/// The engine in this process, behind a lock: what the tests use.
impl EngineHandle for Mutex<Engine> {
    async fn apply(&self, event: Event) -> Result<Vec<Output>, EngineError> {
        self.lock()
            .unwrap_or_else(PoisonError::into_inner)
            .apply(event)
    }

    async fn query(&self, query: Query) -> Reply {
        self.lock()
            .unwrap_or_else(PoisonError::into_inner)
            .query(query)
    }
}

/// The engine's tip.
pub(crate) async fn engine_tip(engine: &impl EngineHandle) -> Option<BlockRef> {
    match engine.query(Query::Tip).await {
        Reply::Tip(tip) => tip,
        other => unreachable!("`Query::Tip` answered {other:?}"),
    }
}

/// The blocks the engine can revert, oldest first.
pub(crate) async fn engine_chain(engine: &impl EngineHandle) -> Vec<ChainLink> {
    match engine.query(Query::Chain).await {
        Reply::Chain(links) => links,
        other => unreachable!("`Query::Chain` answered {other:?}"),
    }
}

/// The best full block `/info` reports, if the node has one: the tip the relay follows.
pub(crate) fn best_full_block(info: &InfoResponse) -> Option<BlockRef> {
    Some(BlockRef { height: info.full_height?, header_id: info.best_full_header_id? })
}

/// How long to wait after a failed call: the node's `Retry-After` on a `503`, or `fallback`.
/// A `503` is a wait, never an error the caller acts on.
pub fn backoff(error: &NodeError, fallback: Duration) -> Duration {
    match error {
        NodeError::Unavailable { retry_after: Some(after) } => *after,
        _ => fallback,
    }
}

#[cfg(test)]
#[path = "mod_tests.rs"]
mod tests;
