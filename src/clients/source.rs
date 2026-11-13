use std::future::Future;

use crate::{
    clients::node::{IndexedHeightResponse, InfoResponse, NodeError},
    types::ergo::{
        Balance, BoxId, ErgoTree, HeaderId, IndexedBlock, IndexedBox, TokenId, TokenInfo, UTxO,
        UnconfirmedTransaction,
    },
};

/// The most boxes the indexer returns per request.
pub const MAX_PAGE_LIMIT: u32 = 16384;

/// A page of an indexer query, oldest boxes first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Page {
    pub offset: u32,
    pub limit: u32,
}

/// The node endpoints the adapters use.
pub trait NodeSource: Send + Sync {
    /// The best full block (the tip the relay follows) and the mempool update hint.
    fn info(&self) -> impl Future<Output = Result<InfoResponse, NodeError>> + Send;

    fn indexed_height(
        &self,
    ) -> impl Future<Output = Result<IndexedHeightResponse, NodeError>> + Send;

    /// `None` unless the indexer has indexed every tx of the block under this header, so an
    /// orphaned block, or one the indexer has not reached, is `None`. The header's `parentId`
    /// is how the block consumer walks back from the best full block to its own chain.
    fn indexed_block(
        &self,
        header_id: &HeaderId,
    ) -> impl Future<Output = Result<Option<IndexedBlock>, NodeError>> + Send;

    /// The mempool, without transactions whose inputs the node could not resolve, nor any of
    /// their descendants. So every input of a listed tx is either created by another listed tx
    /// or was in the node's UTXO set when it answered; the engine's overlay relies on this.
    /// Usually free of double spends, but not always: the node re-adds txs without a
    /// double-spend check (rolled-back txs, and cleanup results racing a replacement).
    fn mempool(
        &self,
    ) -> impl Future<Output = Result<Vec<UnconfirmedTransaction>, NodeError>> + Send;

    fn unspent_by_ergo_tree(
        &self,
        ergo_tree: &ErgoTree,
        page: Page,
    ) -> impl Future<Output = Result<Vec<IndexedBox>, NodeError>> + Send;

    fn unspent_by_token(
        &self,
        token_id: &TokenId,
        page: Page,
    ) -> impl Future<Output = Result<Vec<IndexedBox>, NodeError>> + Send;

    /// `None` if the token does not exist.
    fn token_info(
        &self,
        token_id: &TokenId,
    ) -> impl Future<Output = Result<Option<TokenInfo>, NodeError>> + Send;

    /// Unspent boxes from the UTXO set and the mempool. Missing ids are left out.
    fn boxes_with_pool(
        &self,
        ids: &[BoxId],
    ) -> impl Future<Output = Result<Vec<UTxO>, NodeError>> + Send;

    /// Any box the indexer knows, spent or not. `None` if unknown.
    fn indexed_box(
        &self,
        id: &BoxId,
    ) -> impl Future<Output = Result<Option<IndexedBox>, NodeError>> + Send;

    /// Confirmed balance of the boxes guarded by `ergo_tree`.
    fn confirmed_balance(
        &self,
        ergo_tree: &ErgoTree,
    ) -> impl Future<Output = Result<Balance, NodeError>> + Send;
}
