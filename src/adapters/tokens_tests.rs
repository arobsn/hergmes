//! The token checker against `wiremock`, with bodies built from the recorded tokens.

use serde_json::Value;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{method, path},
};

use super::*;
use crate::{
    clients::NodeClient,
    fixtures::{self, load, sample},
    types::address::Network,
};

fn recorded(name: &str) -> ResponseTemplate {
    ResponseTemplate::new(fixtures::status(name))
        .set_body_raw(fixtures::text(name), "application/json")
}

/// The recorded small-supply token, with its emission replaced.
fn token(emission: u64) -> ResponseTemplate {
    let mut body: Value = load("token_small_supply");
    body["emissionAmount"] = emission.into();
    ResponseTemplate::new(200).set_body_json(body)
}

fn id() -> TokenId {
    sample("smallSupplyToken").parse().unwrap()
}

async fn checker(response: ResponseTemplate, calls: u64) -> (MockServer, TokenChecker<NodeClient>) {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(format!("/blockchain/token/byId/{}", id())))
        .respond_with(response)
        .expect(calls)
        .mount(&server)
        .await;
    let client = NodeClient::new(reqwest::Client::new(), &server.uri(), Network::Mainnet);
    (server, TokenChecker::new(client, SUPPLY_LIMIT))
}

#[tokio::test]
async fn emissions_up_to_the_limit_are_accepted() {
    for emission in [1, 100] {
        let (_server, checker) = checker(token(emission), 1).await;
        checker.check(&id()).await.unwrap();
    }
}

#[tokio::test]
async fn an_emission_above_the_limit_is_rejected() {
    let (_server, checker) = checker(token(101), 1).await;
    let error = checker.check(&id()).await.unwrap_err();
    assert!(matches!(error, TokenError::SupplyTooLarge { emission: 101, limit: 100 }), "{error:?}");
}

#[tokio::test]
async fn the_recorded_tokens_are_judged_by_their_emission() {
    for (name, accepted) in
        [("token_nft", true), ("token_small_supply", true), ("token_large_supply", false)]
    {
        let body: Value = load(name);
        let id: TokenId = body["id"].as_str().unwrap().parse().unwrap();
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(format!("/blockchain/token/byId/{id}")))
            .respond_with(recorded(name))
            .mount(&server)
            .await;
        let client = NodeClient::new(reqwest::Client::new(), &server.uri(), Network::Mainnet);
        let checker = TokenChecker::new(client, SUPPLY_LIMIT);
        assert_eq!(checker.check(&id).await.is_ok(), accepted, "{name}");
    }
}

#[tokio::test]
async fn an_unknown_token_is_rejected_and_asked_again() {
    let (_server, checker) = checker(recorded("token_unknown"), 2).await;
    for _ in 0..2 {
        let error = checker.check(&id()).await.unwrap_err();
        assert!(matches!(error, TokenError::UnknownToken), "{error:?}");
    }
}

#[tokio::test]
async fn a_known_emission_is_read_once() {
    let (_server, checker) = checker(token(101), 1).await;
    for _ in 0..3 {
        assert!(checker.check(&id()).await.is_err());
    }
}

#[tokio::test]
async fn a_503_is_a_wait_not_an_unknown_token() {
    let unavailable = ResponseTemplate::new(503).insert_header("Retry-After", "1");
    let (_server, checker) = checker(unavailable, 1).await;
    let error = checker.check(&id()).await.unwrap_err();
    assert!(matches!(error, TokenError::Node(NodeError::Unavailable { .. })), "{error:?}");
}
