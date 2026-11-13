use std::sync::Arc;

use arc_swap::ArcSwap;
pub use mempool::MempoolSnapshot;
use tokio::task::JoinHandle;

use crate::{clients::NodeClient, error::AppError};

mod mempool;

pub struct Watcher {
    pub mempool_snapshot: Arc<ArcSwap<MempoolSnapshot>>,
    pub handle: JoinHandle<Result<(), AppError>>,
}

/// Starts the mempool watcher in the background and returns immediately.
///
/// The caller owns the task handle and decides how to wait on it.
pub fn spawn(node: NodeClient) -> Watcher {
    let mempool_snapshot = Arc::new(ArcSwap::from_pointee(MempoolSnapshot::default()));

    let cloned_mempool_snapshot = mempool_snapshot.clone();
    let handle = tokio::spawn(async move { mempool::start(&node, cloned_mempool_snapshot).await });

    Watcher { mempool_snapshot, handle }
}
