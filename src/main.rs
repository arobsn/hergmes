use std::{error::Error, time::Duration};

use dotenvy::dotenv;
use hergmes::{
    clients::node::ErgoNodeClient,
    env::ERGO_NODE_URL,
    telemetry::{self, default_subscriber},
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let _ = dotenv();
    telemetry::init(default_subscriber());

    let http_client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()?;
    let node = ErgoNodeClient::new(http_client, &ERGO_NODE_URL);
    node.check_node_index_status().await?;

    Ok(())
}
