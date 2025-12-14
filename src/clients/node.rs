use serde::{self, Deserialize, Serialize};
use tracing::{debug, info};

use crate::types::{
    HashDigest,
    ergo::{Block, BlockHeader, SpendingProof, TransactionInput, UTxO, UnconfirmedTransaction},
};

#[derive(Debug, thiserror::Error)]
pub enum NodeError {
    #[error(transparent)]
    HttpError(#[from] reqwest::Error),

    #[error("The node is not fully indexed.")]
    NotIndexed(IndexedHeightResponse),
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexedHeightResponse {
    pub indexed_height: u64,
    pub full_height: u64,
}

#[derive(Debug, Deserialize)]
pub struct InfoResponse {
    #[serde(rename = "lastMemPoolUpdateTime")]
    pub last_mempool_update: u64,
}

#[derive(Debug, Clone)]
pub struct NodeClient {
    http_client: reqwest::Client,
    base_url: String,
}

#[derive(Debug, Deserialize, Serialize)]
struct MempoolTransactionResponse {
    pub id: HashDigest,
    pub inputs: Vec<MempoolTransactionInput>,
    pub outputs: Vec<UTxO>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct MempoolTransactionInput {
    #[serde(flatten)]
    pub utxo: Option<UTxO>,
    #[serde(rename = "spendingProof")]
    pub spending_proof: SpendingProof,
}

impl From<MempoolTransactionResponse> for UnconfirmedTransaction {
    fn from(mempool_input: MempoolTransactionResponse) -> Self {
        UnconfirmedTransaction {
            id: mempool_input.id,
            outputs: mempool_input.outputs,
            inputs: mempool_input
                .inputs
                .into_iter()
                .map(|input| TransactionInput {
                    utxo: input.utxo.expect("UTxO should be present"),
                    spending_proof: input.spending_proof,
                })
                .collect(),
        }
    }
}

impl NodeClient {
    pub fn new(http_client: reqwest::Client, base_url: &str) -> Self {
        Self { http_client, base_url: base_url.trim_end_matches('/').to_string() }
    }

    #[tracing::instrument(skip(self))]
    pub async fn get_indexed_height(&self) -> Result<IndexedHeightResponse, NodeError> {
        let url = self.build_url("blockchain/indexedHeight");
        let resp = self.http_client.get(&url).send().await?.json().await?;
        Ok(resp)
    }

    #[tracing::instrument(skip(self))]
    pub async fn get_mempool_snapshot(&self) -> Result<Vec<UnconfirmedTransaction>, NodeError> {
        let url = self.build_url("transactions/unconfirmed");
        let resp: Vec<MempoolTransactionResponse> = self
            .http_client
            .get(&url)
            .query(&[("limit", i32::MAX)])
            .send()
            .await?
            .json()
            .await?;

        // Filter out invalid transactions (those with missing UTxOs in inputs)
        // https://github.com/ergoplatform/ergo/issues/2248#issuecomment-3463844934
        let valid = resp
            .into_iter()
            .filter(|utx| utx.inputs.iter().all(|i| i.utxo.is_some()))
            .map(|utx| utx.into())
            .collect::<Vec<UnconfirmedTransaction>>();

        Ok(valid)
    }

    #[tracing::instrument(skip(self))]
    pub async fn get_info(&self) -> Result<InfoResponse, NodeError> {
        let url = self.build_url("info");
        let response: InfoResponse = self.http_client.get(&url).send().await?.json().await?;
        debug!(?response, "Node info fetched.");

        Ok(response)
    }

    #[tracing::instrument(skip(self))]
    pub async fn get_last_mempool_update_timestamp(&self) -> Result<u64, NodeError> {
        let info = self.get_info().await?;
        Ok(info.last_mempool_update)
    }

    #[tracing::instrument(skip(self))]
    pub async fn check_node_index_status(&self) -> Result<(), NodeError> {
        info!("Checking node index status...");
        let index_status = self.get_indexed_height().await?;

        if index_status.indexed_height != index_status.full_height {
            return Err(NodeError::NotIndexed(index_status));
        }

        debug!(?index_status, "Node is fully indexed.");

        Ok(())
    }

    #[tracing::instrument(skip(self))]
    pub async fn get_unconfirmed_transaction_ids(&self) -> Result<Vec<HashDigest>, NodeError> {
        let url = self.build_url("transactions/unconfirmed/transactionIds");
        let resp = self.http_client.get(&url).send().await?.json().await?;
        Ok(resp)
    }

    #[tracing::instrument(skip(self))]
    pub async fn get_unconfirmed_transactions_by_ids(
        &self,
        tx_ids: &[HashDigest],
    ) -> Result<Vec<UnconfirmedTransaction>, NodeError> {
        let url = self.build_url("transactions/unconfirmed/byTransactionIds");
        let resp = self
            .http_client
            .post(&url)
            .json(tx_ids)
            .send()
            .await?
            .json()
            .await?;
        Ok(resp)
    }

    #[tracing::instrument(skip(self))]
    pub async fn get_last_n_headers(&self, n: u32) -> Result<Vec<BlockHeader>, NodeError> {
        let url = self.build_url(&format!("blocks/lastHeaders/{n}"));
        let resp = self.http_client.get(&url).send().await?.json().await?;
        Ok(resp)
    }

    #[tracing::instrument(skip(self))]
    pub async fn get_block(&self, header_id: &str) -> Result<Block, NodeError> {
        let url = self.build_url(&format!("blocks/{header_id}"));
        let resp = self.http_client.get(&url).send().await?.json().await?;
        Ok(resp)
    }

    fn build_url(&self, path: &str) -> String {
        format!("{}/{}", self.base_url, path)
    }
}
