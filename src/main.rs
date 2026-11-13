use std::time::Duration;

use dotenvy::dotenv;
use hergmes::{
    adapters::{MempoolPoller, mempool},
    clients::NodeClient,
    engine::Event,
    env::ERGO_NODE_URL,
    error::AppError,
    trace::{self, default_subscriber},
};
use tokio::sync::mpsc;
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

    let (events, mut inbox) = mpsc::channel(16);
    let poller = MempoolPoller::new(node_client, mempool::Config::default());
    let poller = tokio::spawn(poller.run(events));

    // Until the engine task (step 7) owns the inbox, log what the adapters send.
    while let Some(event) = inbox.recv().await {
        if let Event::MempoolSnapshot { txs, observed_tip } = event {
            info!(txs = txs.len(), height = observed_tip.height, "Mempool snapshot");
        }
    }

    Ok(poller.await?)
}
