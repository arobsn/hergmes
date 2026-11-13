use std::future::Future;

use crate::{
    clients::node::{IndexedHeightResponse, InfoResponse, NodeError},
    types::ergo::{
        Balance, BlockHeader, BoxId, ErgoTree, HeaderId, IndexedBlock, IndexedBox, TokenId,
        TokenInfo, UTxO, UnconfirmedTransaction,
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
    fn info(&self) -> impl Future<Output = Result<InfoResponse, NodeError>> + Send;

    /// The last `count` headers, oldest first.
    fn last_headers(
        &self,
        count: u32,
    ) -> impl Future<Output = Result<Vec<BlockHeader>, NodeError>> + Send;

    fn indexed_height(
        &self,
    ) -> impl Future<Output = Result<IndexedHeightResponse, NodeError>> + Send;

    /// `None` if the indexer does not know the block.
    fn indexed_block(
        &self,
        header_id: &HeaderId,
    ) -> impl Future<Output = Result<Option<IndexedBlock>, NodeError>> + Send;

    /// The mempool, without transactions whose inputs the node could not resolve.
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
