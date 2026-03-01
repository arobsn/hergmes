use std::time::Duration;

use dotenvy::dotenv;
use hergmes::{
    clients::node::NodeClient,
    env::ERGO_NODE_URL,
    error::AppError,
    trace::{self, default_subscriber},
    watcher,
};
use tracing::warn;

#[tokio::main]
async fn main() -> Result<(), AppError> {
    if let Err(e) = dotenv() {
        warn!("Failed to load .env file: {:?}. Using environment variables instead.", e);
    }
    trace::init(default_subscriber());

    let http_client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .expect("Failed to build HTTP client");

    let node = NodeClient::new(http_client, &ERGO_NODE_URL);
    node.check_node_index_status().await?;

    let _mempool_snapshot = watcher::spawn(node.clone()).await?;

    Ok(())
}
