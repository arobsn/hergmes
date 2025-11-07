use std::time::Duration;

use dotenvy::dotenv;
use hergmes::{clients::node::NodeClient, env::ERGO_NODE_URL, types::ergo::Base58String};

#[tokio::test]
async fn test_node_balance() {
    let _ = dotenv();

    let http_client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .unwrap();

    let node = NodeClient::new(http_client, &ERGO_NODE_URL);

    let address = Base58String("9hMDjzgnrwET8dweNnK3wKHJf7Vi3zWcKsFEEcdETdSie34BQ16".to_string());

    let balance = node.get_balance(&address).await.unwrap();

    assert!(balance.confirmed.nano_ergs >= 10);
}
