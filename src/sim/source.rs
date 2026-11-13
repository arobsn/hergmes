//! [`SimChain`]: a shared [`Chain`] served through [`NodeSource`].

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use crate::{
    clients::{
        NodeError, NodeSource, Page,
        node::{IndexedHeightResponse, InfoResponse},
    },
    sim::Chain,
    types::ergo::{
        Balance, BlockHeader, BoxId, ErgoTree, HeaderId, IndexedBlock, IndexedBox, TokenId,
        TokenInfo, UTxO, UnconfirmedTransaction,
    },
};

/// The [`NodeSource`] methods, for the call log and hooks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Endpoint {
    Info,
    LastHeaders,
    IndexedHeight,
    IndexedBlock,
    Mempool,
    UnspentByErgoTree,
    UnspentByToken,
    TokenInfo,
    BoxesWithPool,
    IndexedBox,
    ConfirmedBalance,
}

impl Endpoint {
    /// Answers `503` while the indexer recovers: every `/blockchain` endpoint except
    /// `indexedHeight`, which the node serves before its recovery check.
    pub fn gated_by_recovery(self) -> bool {
        !matches!(
            self,
            Self::Info
                | Self::LastHeaders
                | Self::Mempool
                | Self::BoxesWithPool
                | Self::IndexedHeight
        )
    }
}

/// Runs before an endpoint answers, with the chain.
pub type Hook = Box<dyn FnMut(Endpoint, &mut Chain) + Send>;

/// A shared handle on a [`Chain`]: adapters read it through [`NodeSource`] while the test
/// drives it through [`SimChain::lock`]. Clones share the chain. Every call locks it briefly
/// and never across an `.await`, so it answers immediately.
#[derive(Clone)]
pub struct SimChain(Arc<Mutex<Chain>>);

impl SimChain {
    pub fn new(chain: Chain) -> Self {
        Self(Arc::new(Mutex::new(chain)))
    }

    /// The chain, to drive or inspect. Don't hold the guard while calling a [`NodeSource`]
    /// method on the same handle: that deadlocks.
    pub fn lock(&self) -> MutexGuard<'_, Chain> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Runs `hook` before every endpoint call, e.g. to mine a block between two page reads.
    /// Replaces any previous hook.
    pub fn on_call(&self, hook: impl FnMut(Endpoint, &mut Chain) + Send + 'static) {
        self.lock().hook = Some(Box::new(hook));
    }

    /// The endpoints called so far, in order.
    pub fn calls(&self) -> Vec<Endpoint> {
        self.lock().calls.clone()
    }

    fn serve<T>(
        &self,
        endpoint: Endpoint,
        answer: impl FnOnce(&Chain) -> Result<T, NodeError>,
    ) -> Result<T, NodeError> {
        let mut chain = self.lock();
        chain.calls.push(endpoint);
        if let Some(mut hook) = chain.hook.take() {
            hook(endpoint, &mut chain);
            if chain.hook.is_none() {
                chain.hook = Some(hook);
            }
        }
        if endpoint.gated_by_recovery() {
            chain.indexer_gate()?;
        }
        answer(&chain)
    }
}

impl NodeSource for SimChain {
    async fn info(&self) -> Result<InfoResponse, NodeError> {
        self.serve(Endpoint::Info, |c| Ok(c.info()))
    }

    async fn last_headers(&self, count: u32) -> Result<Vec<BlockHeader>, NodeError> {
        self.serve(Endpoint::LastHeaders, |c| Ok(c.last_headers(count)))
    }

    async fn indexed_height(&self) -> Result<IndexedHeightResponse, NodeError> {
        self.serve(Endpoint::IndexedHeight, |c| Ok(c.indexed_height()))
    }

    async fn indexed_block(&self, header_id: &HeaderId) -> Result<Option<IndexedBlock>, NodeError> {
        self.serve(Endpoint::IndexedBlock, |c| Ok(c.indexed_block(header_id)))
    }

    async fn mempool(&self) -> Result<Vec<UnconfirmedTransaction>, NodeError> {
        self.serve(Endpoint::Mempool, |c| Ok(c.mempool()))
    }

    async fn unspent_by_ergo_tree(
        &self,
        ergo_tree: &ErgoTree,
        page: Page,
    ) -> Result<Vec<IndexedBox>, NodeError> {
        self.serve(Endpoint::UnspentByErgoTree, |c| {
            c.indexed_unspent(|b| b.ergo_tree == *ergo_tree, page)
        })
    }

    async fn unspent_by_token(
        &self,
        token_id: &TokenId,
        page: Page,
    ) -> Result<Vec<IndexedBox>, NodeError> {
        self.serve(Endpoint::UnspentByToken, |c| {
            c.indexed_unspent(|b| b.tokens.iter().any(|t| t.id == *token_id), page)
        })
    }

    async fn token_info(&self, token_id: &TokenId) -> Result<Option<TokenInfo>, NodeError> {
        self.serve(Endpoint::TokenInfo, |c| Ok(c.token_info(token_id)))
    }

    async fn boxes_with_pool(&self, ids: &[BoxId]) -> Result<Vec<UTxO>, NodeError> {
        self.serve(Endpoint::BoxesWithPool, |c| Ok(c.boxes_with_pool(ids)))
    }

    async fn indexed_box(&self, id: &BoxId) -> Result<Option<IndexedBox>, NodeError> {
        self.serve(Endpoint::IndexedBox, |c| Ok(c.indexed_box(id)))
    }

    async fn confirmed_balance(&self, ergo_tree: &ErgoTree) -> Result<Balance, NodeError> {
        self.serve(Endpoint::ConfirmedBalance, |c| Ok(c.confirmed_balance(ergo_tree)))
    }
}

#[cfg(test)]
#[path = "source_tests.rs"]
mod tests;
