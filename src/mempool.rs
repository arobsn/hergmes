use std::{sync::Arc, time::Duration};

use arc_swap::ArcSwap;
use tokio::time::sleep;
use tracing::{error, info};

use crate::{clients::node::NodeClient, error::AppError, types::ergo::UnconfirmedTransaction};

pub struct MempoolSnapshot {
    pub last_update: u64,
    pub transactions: Vec<UnconfirmedTransaction>,
}

#[tracing::instrument(skip(node, swap))]
pub async fn start_indexer(
    node: &NodeClient,
    swap: Arc<ArcSwap<MempoolSnapshot>>,
) -> Result<(), AppError> {
    info!("Starting mempool indexer...");

    let mut last_update = 0u64;
    loop {
        match node.get_last_mempool_update_timestamp().await {
            Ok(updated) if updated > last_update => match node.get_mempool_snapshot().await {
                Ok(transactions) => {
                    last_update = updated;
                    info!(count = ?transactions.len(), ?last_update, "Mempool updated, storing new snapshot");
                    swap.store(Arc::new(MempoolSnapshot {
                        last_update,
                        transactions,
                    }));
                }
                Err(e) => error!("Error fetching mempool snapshot: {:?}", e),
            },
            Err(e) => error!("Error fetching mempool update timestamp: {:?}", e),
            _ => {}
        }

        sleep(Duration::from_secs(1)).await;
    }
}
