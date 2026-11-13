use std::time::Duration;

use reqwest::{RequestBuilder, Response, StatusCode, header::RETRY_AFTER};
use serde::{Deserialize, de::DeserializeOwned};

use crate::{
    clients::source::{NodeSource, Page},
    types::{
        address::{self, Network, UnknownNetwork},
        ergo::{
            Balance, BalanceResponse, BoxId, ErgoTree, HeaderId, IndexedBlock, IndexedBox, TokenId,
            TokenInfo, TxId, UTxO, UnconfirmedTransaction,
        },
    },
};

#[derive(Debug, thiserror::Error)]
pub enum NodeError {
    #[error(transparent)]
    HttpError(#[from] reqwest::Error),

    #[error("The node is unavailable (503), retry after {retry_after:?}.")]
    Unavailable { retry_after: Option<Duration> },

    #[error("The node returned {status}: {reason} ({detail:?}).")]
    Api { status: StatusCode, reason: String, detail: Option<String> },

    #[error("The node is not fully indexed.")]
    NotIndexed(IndexedHeightResponse),

    #[error(transparent)]
    UnknownNetwork(#[from] UnknownNetwork),
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexedHeightResponse {
    pub indexed_height: u32,
    pub full_height: u32,
}

/// Error body returned by the node, e.g. `{"error": 404, "reason": "not-found", "detail": null}`.
#[derive(Debug, Deserialize)]
pub struct ApiError {
    pub reason: String,
    pub detail: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct InfoResponse {
    /// Wall-clock time (ms) of the last `ChangedMempool` event: set at launch, and on every
    /// applied block, accepted or invalidated tx, and cleanup recheck, changed or not. It is a
    /// hint, not a version: it is set by a different actor than the one the mempool endpoints
    /// read, so a read right after it moves can still return the previous pool, and it can
    /// move backwards if the node's clock does.
    #[serde(rename = "lastMemPoolUpdateTime")]
    pub last_mempool_update: u64,
    pub network: String,
    /// The best full block: the tip of the chain the node's UTXO set, mempool and indexer
    /// follow. Absent before the first full block. Not the best header, which can run ahead of
    /// it (`/blocks/lastHeaders` follows headers).
    #[serde(rename = "bestFullHeaderId")]
    pub best_full_header_id: Option<HeaderId>,
    #[serde(rename = "fullHeight")]
    pub full_height: Option<u32>,
}

#[derive(Debug, Clone)]
pub struct NodeClient {
    http_client: reqwest::Client,
    base_url: String,
    network: Network,
}

#[derive(Debug, Deserialize)]
struct MempoolTransactionResponse {
    id: TxId,
    inputs: Vec<MempoolTransactionInput>,
    outputs: Vec<UTxO>,
}

#[derive(Debug, Deserialize)]
struct MempoolTransactionInput {
    #[serde(flatten)]
    utxo: Option<UTxO>,
}

impl MempoolTransactionResponse {
    /// `None` if the node could not resolve any of its inputs.
    fn resolve(self) -> Option<UnconfirmedTransaction> {
        let inputs = self
            .inputs
            .into_iter()
            .map(|i| i.utxo)
            .collect::<Option<_>>()?;
        Some(UnconfirmedTransaction { id: self.id, inputs, outputs: self.outputs })
    }
}

/// Drops transactions with any input whose UTxO the node could not resolve.
///
/// See <https://github.com/ergoplatform/ergo/issues/2248#issuecomment-3463844934>.
fn filter_out_unresolved(txs: Vec<MempoolTransactionResponse>) -> Vec<UnconfirmedTransaction> {
    txs.into_iter()
        .filter_map(MempoolTransactionResponse::resolve)
        .collect()
}

impl NodeClient {
    pub fn new(http_client: reqwest::Client, base_url: &str, network: Network) -> Self {
        Self { http_client, base_url: base_url.trim_end_matches('/').to_string(), network }
    }

    /// Builds a client for the network the node reports.
    pub async fn connect(http_client: reqwest::Client, base_url: &str) -> Result<Self, NodeError> {
        let mut client = Self::new(http_client, base_url, Network::Mainnet);
        client.network = client.info().await?.network.parse()?;
        Ok(client)
    }

    pub fn network(&self) -> Network {
        self.network
    }

    fn get(&self, path: &str) -> RequestBuilder {
        self.http_client.get(format!("{}/{}", self.base_url, path))
    }

    fn post(&self, path: &str) -> RequestBuilder {
        self.http_client.post(format!("{}/{}", self.base_url, path))
    }

    fn unspent_page(request: RequestBuilder, page: Page) -> RequestBuilder {
        request.query(&[
            ("offset", page.offset.to_string()),
            ("limit", page.limit.to_string()),
            ("sortDirection", "asc".to_string()),
        ])
    }
}

/// Sends a request and parses a 2xx body.
async fn fetch<T: DeserializeOwned>(request: RequestBuilder) -> Result<T, NodeError> {
    let response = check(request.send().await?).await?;
    Ok(response.json().await?)
}

/// Like [`fetch`], but a 404 is `None`.
async fn fetch_optional<T: DeserializeOwned>(
    request: RequestBuilder,
) -> Result<Option<T>, NodeError> {
    let response = request.send().await?;
    if response.status() == StatusCode::NOT_FOUND {
        return Ok(None);
    }
    Ok(Some(check(response).await?.json().await?))
}

async fn check(response: Response) -> Result<Response, NodeError> {
    let status = response.status();
    if status.is_success() {
        return Ok(response);
    }
    if status == StatusCode::SERVICE_UNAVAILABLE {
        let retry_after = response
            .headers()
            .get(RETRY_AFTER)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.trim().parse().ok())
            .map(Duration::from_secs);
        return Err(NodeError::Unavailable { retry_after });
    }

    let body = response.text().await.unwrap_or_default();
    let (reason, detail) = match serde_json::from_str::<ApiError>(&body) {
        Ok(e) => (e.reason, e.detail),
        Err(_) => (body, None),
    };
    Err(NodeError::Api { status, reason, detail })
}

impl NodeSource for NodeClient {
    #[tracing::instrument(skip(self))]
    async fn info(&self) -> Result<InfoResponse, NodeError> {
        fetch(self.get("info")).await
    }

    #[tracing::instrument(skip(self))]
    async fn indexed_height(&self) -> Result<IndexedHeightResponse, NodeError> {
        fetch(self.get("blockchain/indexedHeight")).await
    }

    #[tracing::instrument(skip(self))]
    async fn indexed_block(&self, header_id: &HeaderId) -> Result<Option<IndexedBlock>, NodeError> {
        fetch_optional(self.get(&format!("blockchain/block/byHeaderId/{header_id}"))).await
    }

    #[tracing::instrument(skip(self))]
    async fn mempool(&self) -> Result<Vec<UnconfirmedTransaction>, NodeError> {
        let request = self
            .get("transactions/unconfirmed")
            .query(&[("limit", i32::MAX)]);
        Ok(filter_out_unresolved(fetch(request).await?))
    }

    #[tracing::instrument(skip(self))]
    async fn unspent_by_ergo_tree(
        &self,
        ergo_tree: &ErgoTree,
        page: Page,
    ) -> Result<Vec<IndexedBox>, NodeError> {
        let request = self
            .post("blockchain/box/unspent/byErgoTree")
            .json(ergo_tree);
        Ok(fetch_optional(Self::unspent_page(request, page))
            .await?
            .unwrap_or_default())
    }

    #[tracing::instrument(skip(self))]
    async fn unspent_by_token(
        &self,
        token_id: &TokenId,
        page: Page,
    ) -> Result<Vec<IndexedBox>, NodeError> {
        let request = self.get(&format!("blockchain/box/unspent/byTokenId/{token_id}"));
        Ok(fetch_optional(Self::unspent_page(request, page))
            .await?
            .unwrap_or_default())
    }

    #[tracing::instrument(skip(self))]
    async fn token_info(&self, token_id: &TokenId) -> Result<Option<TokenInfo>, NodeError> {
        fetch_optional(self.get(&format!("blockchain/token/byId/{token_id}"))).await
    }

    #[tracing::instrument(skip(self, ids), fields(count = ids.len()))]
    async fn boxes_with_pool(&self, ids: &[BoxId]) -> Result<Vec<UTxO>, NodeError> {
        fetch(self.post("utxo/withPool/byIds").json(ids)).await
    }

    #[tracing::instrument(skip(self))]
    async fn indexed_box(&self, id: &BoxId) -> Result<Option<IndexedBox>, NodeError> {
        fetch_optional(self.get(&format!("blockchain/box/byId/{id}"))).await
    }

    #[tracing::instrument(skip(self))]
    async fn confirmed_balance(&self, ergo_tree: &ErgoTree) -> Result<Balance, NodeError> {
        let address = address::encode(&ergo_tree.0, self.network);
        let response: BalanceResponse =
            fetch(self.post("blockchain/balance").json(&address)).await?;
        Ok(response.confirmed)
    }
}

#[cfg(test)]
#[path = "node_tests.rs"]
mod tests;
