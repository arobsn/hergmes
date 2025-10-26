use tracing::info;

use crate::{clients::node::NodeClient, error::AppError};

#[tracing::instrument(skip(node))]
pub async fn start_indexer(node: &NodeClient) -> Result<(), AppError> {
    info!("Starting mempool indexer...");

    let txns = node.get_mempool_transactions().await?;
    info!(
        len = ?txns.len(),
        "Fetched mempool transactions."
    );

    Ok(())
}
