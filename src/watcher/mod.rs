use std::sync::Arc;

use arc_swap::ArcSwap;
pub use mempool::MempoolSnapshot;

use crate::{clients::node::NodeClient, error::AppError};

mod mempool;

pub async fn spawn(node: NodeClient) -> Result<Arc<ArcSwap<MempoolSnapshot>>, AppError> {
    let mempool_snapshot = Arc::new(ArcSwap::from_pointee(MempoolSnapshot::default()));

    let cloned_mempool_snapshot = mempool_snapshot.clone();
    let _ = tokio::spawn(async move { mempool::start(&node, cloned_mempool_snapshot).await }).await;

    Ok(mempool_snapshot)
}
