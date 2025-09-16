use std::error::Error;

use dotenvy::dotenv;
use hergmes::{
    clients::node::ErgoNodeClient,
    telemetry::{self, default_subscriber},
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    dotenv()?;
    telemetry::init(default_subscriber());

    let http_client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()?;
    let node = ErgoNodeClient::new(http_client, &std::env::var("ERGO_NODE_URL")?);
    node.check_node_index_status().await?;

    Ok(())
}
