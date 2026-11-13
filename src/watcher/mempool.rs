use std::{sync::Arc, time::Duration};

use arc_swap::ArcSwap;
use tokio::time::sleep;
use tracing::{error, info};

use crate::{
    clients::{NodeClient, NodeSource},
    error::AppError,
    types::ergo::UnconfirmedTransaction,
};

#[derive(Default)]
pub struct MempoolSnapshot {
    pub last_update: u64,
    pub transactions: Vec<UnconfirmedTransaction>,
}

const POLLING_INTERVAL: Duration = Duration::from_secs(1);

#[tracing::instrument(skip(node, swap))]
pub async fn start(node: &NodeClient, swap: Arc<ArcSwap<MempoolSnapshot>>) -> Result<(), AppError> {
    info!("Starting mempool indexer...");

    let mut last_update = 0u64;
    loop {
        match node.info().await.map(|info| info.last_mempool_update) {
            Ok(updated) if updated > last_update => match node.mempool().await {
                Ok(transactions) => {
                    last_update = updated;
                    info!(count = ?transactions.len(), ?last_update, "Mempool updated, storing new snapshot");
                    swap.store(Arc::new(MempoolSnapshot { last_update, transactions }));
                }
                Err(error) => error!(?error, "Error fetching mempool snapshot"),
            },
            Err(error) => error!(?error, "Error fetching mempool update timestamp"),
            _ => {}
        }

        sleep(POLLING_INTERVAL).await;
    }
}
