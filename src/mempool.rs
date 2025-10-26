use tracing::info;

use crate::{clients::node::NodeClient, error::AppError};

#[tracing::instrument(skip(node))]
pub async fn start_indexer(node: &NodeClient) -> Result<(), AppError> {
    info!("Starting mempool indexer...");

    let last_update = node.get_last_mempool_update_timestamp().await?;
    let txns = node.get_mempool_transactions().await?;
    info!(
        len = ?txns.len(),
        ?last_update,
        "Fetched mempool transactions."
    );

    Ok(())
}
