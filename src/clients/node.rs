use tracing::{debug, error, info};

use self::types::IndexedHeightResponse;

pub mod types;

#[derive(Debug, thiserror::Error)]
pub enum NodeError {
    #[error(transparent)]
    HttpError(#[from] reqwest::Error),

    #[error("The node is not fully indexed.")]
    NotIndexed(IndexedHeightResponse),
}

#[derive(Debug, Clone)]
pub struct NodeClient {
    http_client: reqwest::Client,
    base_url: String,
}

impl NodeClient {
    pub fn new(http_client: reqwest::Client, base_url: &str) -> Self {
        Self {
            http_client,
            base_url: base_url.trim_end_matches('/').to_string(),
        }
    }

    pub async fn get_indexed_height(&self) -> Result<IndexedHeightResponse, NodeError> {
        let url = self.build_url("blockchain/indexedHeight");
        let resp = self.http_client.get(&url).send().await?.json().await?;
        Ok(resp)
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

    fn build_url(&self, path: &str) -> String {
        format!("{}/{}", self.base_url, path)
    }
}
