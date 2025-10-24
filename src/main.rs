use std::time::Duration;

use dotenvy::dotenv;
use hergmes::{
    clients::node::NodeClient,
    env::ERGO_NODE_URL,
    error::AppError,
    tracing::{self, default_subscriber},
};

#[tokio::main]
async fn main() -> Result<(), AppError> {
    let _ = dotenv();
    tracing::init(default_subscriber());

    let http_client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .expect("Failed to build HTTP client");

    let node = NodeClient::new(http_client, &ERGO_NODE_URL);
    node.check_node_index_status().await?;

    Ok(())
}
