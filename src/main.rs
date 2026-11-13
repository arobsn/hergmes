use std::{sync::Mutex, time::Duration};

use dotenvy::dotenv;
use hergmes::{
    adapters::{Follower, follower},
    clients::NodeClient,
    engine::{self, Engine},
    env::ERGO_NODE_URL,
    error::AppError,
    trace::{self, default_subscriber},
};
use tracing::{info, warn};

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

    let node_client = NodeClient::connect(http_client, &ERGO_NODE_URL).await?;

    // Until step 7's engine task, the engine lives behind a lock in this task.
    let engine = Mutex::new(Engine::new(engine::Config::default()));
    info!(network = ?node_client.network(), "Following the node");
    Follower::new(node_client, follower::Config::default())
        .run(&engine)
        .await;
    Ok(())
}
