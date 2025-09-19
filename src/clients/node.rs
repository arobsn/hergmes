use std::error::Error;

use tracing::{error, info};

use crate::clients::node::types::IndexedHeightResponse;

pub mod types;

#[derive(Debug, Clone)]
pub struct ErgoNodeClient {
    http_client: reqwest::Client,
    base_url: String,
}

impl ErgoNodeClient {
    pub fn new(http_client: reqwest::Client, base_url: &str) -> Self {
        Self {
            http_client,
            base_url: base_url.trim_end_matches('/').to_string(),
        }
    }

    pub async fn get_indexed_height(&self) -> Result<IndexedHeightResponse, Box<dyn Error>> {
        let url = self.build_url("blockchain/indexedHeight");
        let resp = self.http_client.get(&url).send().await?.json().await?;
        Ok(resp)
    }

    #[tracing::instrument(skip(self))]
    pub async fn check_node_index_status(&self) -> Result<(), Box<dyn Error>> {
        info!("Checking Ergo Node index status...");
        let index_status = self.get_indexed_height().await?;

        if index_status.indexed_height != index_status.full_height {
            error!("The Ergo Node is not fully indexed.");
            return Err(format!(
                "The Ergo Node is not fully indexed. Indexed height: {}, Full height: {}",
                index_status.indexed_height, index_status.full_height
            )
            .into());
        }

        Ok(())
    }

    fn build_url(&self, path: &str) -> String {
        format!("{}/{}", self.base_url, path)
    }
}
