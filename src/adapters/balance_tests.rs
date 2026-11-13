//! The balance reader against `wiremock`: the height it reports, and an indexer moving under
//! it.

use serde_json::json;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{method, path},
};

use super::*;
use crate::{
    clients::NodeClient,
    fixtures::{self, sample},
    types::{HexBytes, address::Network},
};

fn height(indexed: u32) -> ResponseTemplate {
    ResponseTemplate::new(200)
        .set_body_json(json!({ "indexedHeight": indexed, "fullHeight": indexed }))
}

/// Serves the indexed heights in order, then the recorded balance.
async fn reader(heights: &[u32]) -> (MockServer, BalanceReader<NodeClient>) {
    let server = MockServer::start().await;
    for &h in heights {
        Mock::given(method("GET"))
            .and(path("/blockchain/indexedHeight"))
            .respond_with(height(h))
            .up_to_n_times(1)
            .mount(&server)
            .await;
    }
    Mock::given(method("POST"))
        .and(path("/blockchain/balance"))
        .respond_with(
            ResponseTemplate::new(fixtures::status("balance"))
                .set_body_raw(fixtures::text("balance"), "application/json"),
        )
        .mount(&server)
        .await;
    let client = NodeClient::new(reqwest::Client::new(), &server.uri(), Network::Mainnet);
    (server, BalanceReader::new(client))
}

fn tree() -> ErgoTree {
    HexBytes(hex::decode(sample("ergoTree")).unwrap())
}

#[tokio::test]
async fn a_balance_comes_with_the_height_it_was_read_at() {
    let (_server, reader) = reader(&[10, 10]).await;
    let read = reader.read(&tree()).await.unwrap().unwrap();
    assert_eq!(read.height, 10);
    assert_eq!(read.confirmed.nano_ergs, 361_490_634_617);
}

#[tokio::test]
async fn an_indexer_moving_during_the_call_is_read_again() {
    let (server, reader) = reader(&[10, 11, 11, 11]).await;
    assert_eq!(reader.read(&tree()).await.unwrap().unwrap().height, 11);
    let balance_calls = server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| r.url.path() == "/blockchain/balance")
        .count();
    assert_eq!(balance_calls, 2);
}

#[tokio::test]
async fn an_indexer_that_keeps_moving_gives_no_balance() {
    let (_server, reader) = reader(&[10, 11, 12, 13, 14, 15]).await;
    assert_eq!(reader.read(&tree()).await.unwrap(), None);
}
