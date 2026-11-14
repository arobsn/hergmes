//! Adapters: all I/O against the node.
//!
//! Block consumer, mempool poller, first-load loader, token checker, box fetcher and balance
//! reader. They fetch, stamp with the observed tip and send events to the engine. Each one
//! has a core that never sleeps (one sync, one poll, one load attempt), so tests drive it
//! against `SimChain` step by step; waiting and back-off live in thin loops around it.

use std::{future::Future, time::Duration};

use crate::{
    clients::{NodeError, node::InfoResponse},
    engine::{BlockRef, ChainLink, EngineError, Event, Output, Query, Reply},
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

/// How adapters reach the engine: an event with its answer, or a query with its reply.
///
/// The block consumer needs the answer to an event (an `EngineError` makes it start again
/// from the engine's chain), and so does the loader (`LoadRejected` makes it retry), so
/// events are not fire-and-forget. The engine task (`runtime::EngineTask`) implements this
/// over its inbox. It answers an applied event with only the outputs addressed to its caller
/// (`LoadRejected`): the rest go to subscribers, and sending them back too would copy every
/// routed output for a caller that drops them.
pub trait EngineHandle: Send + Sync {
    fn apply(
        &self,
        event: Event,
    ) -> impl Future<Output = Result<Result<Vec<Output>, EngineError>, EngineGone>> + Send;

    fn query(&self, query: Query) -> impl Future<Output = Result<Reply, EngineGone>> + Send;
}

/// The engine is gone: its task stopped, which only a panic does while a handle exists.
///
/// A panic can leave the engine half-changed, so there is nothing to retry. Adapters pass
/// this up to `main`, which exits; a restart reloads every key from the node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("the engine task has stopped")]
pub struct EngineGone;

/// A failure of an adapter call that reaches both the node and the engine.
#[derive(Debug, thiserror::Error)]
pub enum AdapterError {
    /// A node call failed. The caller waits and tries again.
    #[error(transparent)]
    Node(#[from] NodeError),
    #[error(transparent)]
    EngineGone(#[from] EngineGone),
}

impl AdapterError {
    /// The node error, for callers that report it apart from a stopped engine, which they
    /// pass up.
    pub fn into_node(self) -> Result<NodeError, EngineGone> {
        match self {
            Self::Node(error) => Ok(error),
            Self::EngineGone(gone) => Err(gone),
        }
    }
}

/// The engine in this process, behind a lock: for tests, which drive adapters without
/// spawning the engine task. Not for production, where the engine task serializes events
/// and routes their outputs in one place.
///
/// A panic halfway through `apply` can leave the engine half-changed, so a poisoned lock is
/// not recovered: the next step of the test fails instead of running on that state.
#[cfg(any(test, feature = "sim"))]
impl EngineHandle for std::sync::Mutex<crate::engine::Engine> {
    async fn apply(&self, event: Event) -> Result<Result<Vec<Output>, EngineError>, EngineGone> {
        Ok(self.lock().expect("the engine panicked").apply(event))
    }

    async fn query(&self, query: Query) -> Result<Reply, EngineGone> {
        Ok(self.lock().expect("the engine panicked").query(query))
    }
}

/// The engine's tip.
pub(crate) async fn engine_tip(engine: &impl EngineHandle) -> Result<Option<BlockRef>, EngineGone> {
    match engine.query(Query::Tip).await? {
        Reply::Tip(tip) => Ok(tip),
        other => unreachable!("`Query::Tip` answered {other:?}"),
    }
}

/// The blocks the engine can revert, oldest first.
pub(crate) async fn engine_chain(engine: &impl EngineHandle) -> Result<Vec<ChainLink>, EngineGone> {
    match engine.query(Query::Chain).await? {
        Reply::Chain(links) => Ok(links),
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
