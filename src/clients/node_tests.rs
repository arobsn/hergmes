//! Recorded node responses: their shapes, and `NodeClient` reading them over HTTP.

use serde_json::Value;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{body_json, method, path, query_param},
};

use super::*;
use crate::{
    fixtures::{self, load, manifest, sample, status},
    types::{HashDigest, HexBytes, address::Address},
};

fn holds(b: &IndexedBox, token_id: &str) -> Option<u64> {
    b.utxo
        .tokens
        .iter()
        .find(|t| t.id.to_string() == token_id)
        .map(|t| t.amount)
}

fn digest(hex: &str) -> HashDigest {
    hex.parse().unwrap()
}

fn sample_tree() -> ErgoTree {
    HexBytes(hex::decode(sample("ergoTree")).unwrap())
}

fn assert_not_found(name: &str) {
    assert_eq!(status(name), 404);
    let err: ApiError = load(name);
    assert_eq!(err.reason, "not-found");
}

mod shapes {
    use super::*;

    #[test]
    fn every_manifest_entry_has_a_file() {
        let manifest = manifest();
        let fixtures = manifest["fixtures"].as_object().expect("fixtures map");
        assert!(!fixtures.is_empty());
        for name in fixtures.keys() {
            assert!(fixtures::path(name).exists(), "{name}.json missing");
        }
    }

    #[test]
    fn info() {
        let info: InfoResponse = load("info");
        assert!(info.last_mempool_update > 0);
        assert_eq!(info.network.parse::<Network>().unwrap(), Network::Mainnet);
    }

    #[test]
    fn indexed_height() {
        let h: IndexedHeightResponse = load("indexed_height");
        assert_eq!(h.indexed_height, h.full_height);
    }

    #[test]
    fn last_headers_are_oldest_first_and_linked() {
        let headers: Vec<BlockHeader> = load("last_headers");
        assert_eq!(headers.len(), 3);
        for pair in headers.windows(2) {
            assert_eq!(pair[1].parent_id, pair[0].id);
            assert_eq!(pair[1].height, pair[0].height + 1);
        }
    }

    #[test]
    fn indexed_block() {
        let block: IndexedBlock = load("indexed_block");
        assert_eq!(block.header.id, digest(&sample("headerId")));
        assert!(!block.transactions.is_empty());

        let mut spends_own_output = false;
        for (i, tx) in block.transactions.iter().enumerate() {
            assert!(!tx.inputs.is_empty());
            for input in &tx.inputs {
                assert!(input.inclusion_height <= block.header.height);
                if input.inclusion_height == block.header.height {
                    let creator = input.utxo.transaction_id;
                    assert!(block.transactions[..i].iter().any(|t| t.id == creator));
                    spends_own_output = true;
                }
            }
            for output in &tx.outputs {
                assert_eq!(output.utxo.transaction_id, tx.id);
                assert_eq!(output.inclusion_height, block.header.height);
            }
        }
        assert!(spends_own_output, "sample block has no box created and spent in it");
    }

    #[test]
    fn indexed_block_unknown() {
        assert_not_found("indexed_block_unknown");
    }

    #[test]
    fn mempool_keeps_resolved_transactions() {
        let txs: Vec<MempoolTransactionResponse> = load("mempool");
        let count = txs.len();
        assert!(count > 0);
        assert_eq!(filter_out_unresolved(txs).len(), count);
    }

    #[test]
    fn mempool_drops_transactions_with_unresolved_inputs() {
        let txs: Vec<MempoolTransactionResponse> = load("mempool_unresolved_input");
        assert!(txs[0].inputs[0].utxo.is_none());
        let dropped = txs[0].id;
        let count = txs.len();

        let kept = filter_out_unresolved(txs);
        assert_eq!(kept.len(), count - 1);
        assert!(kept.iter().all(|tx| tx.id != dropped));
    }

    #[test]
    fn unspent_by_ergo_tree() {
        let boxes: Vec<IndexedBox> = load("unspent_by_ergo_tree");
        assert!(boxes.len() >= 2);
        let address = Address::decode(&sample("address")).unwrap();
        assert_eq!(address.ergo_tree(), &sample_tree());
        assert!(boxes.iter().all(|b| b.utxo.ergo_tree == sample_tree()));
        assert!(
            boxes
                .windows(2)
                .all(|w| w[0].inclusion_height <= w[1].inclusion_height)
        );
    }

    #[test]
    fn unspent_by_ergo_tree_empty_is_200_not_404() {
        assert_eq!(status("unspent_by_ergo_tree_empty"), 200);
        let boxes: Vec<IndexedBox> = load("unspent_by_ergo_tree_empty");
        assert!(boxes.is_empty());
    }

    #[test]
    fn balance() {
        let balance: BalanceResponse = load("balance");
        assert!(balance.confirmed.nano_ergs > 0);

        let empty: BalanceResponse = load("balance_empty");
        assert_eq!(empty.confirmed.nano_ergs, 0);
        assert!(empty.confirmed.tokens.is_empty());
    }

    #[test]
    fn token_info() {
        let nft: TokenInfo = load("token_nft");
        assert_eq!(nft.emission_amount, 1);

        let small: TokenInfo = load("token_small_supply");
        assert!((2..=100).contains(&small.emission_amount));

        let large: TokenInfo = load("token_large_supply");
        assert!(large.emission_amount > 100);

        assert_not_found("token_unknown");
    }

    #[test]
    fn unspent_by_token_nft_is_one_box() {
        let boxes: Vec<IndexedBox> = load("unspent_by_token_nft");
        assert_eq!(boxes.len(), 1);
        assert_eq!(holds(&boxes[0], &sample("nft")), Some(1));
    }

    #[test]
    fn unspent_by_token_small_supply_is_bounded_by_emission() {
        let boxes: Vec<IndexedBox> = load("unspent_by_token_small_supply");
        let info: TokenInfo = load("token_small_supply");
        let token = sample("smallSupplyToken");

        assert!(boxes.len() >= 2);
        assert!(boxes.len() as u64 <= info.emission_amount);
        let total: u64 = boxes
            .iter()
            .map(|b| holds(b, &token).expect("box holds the token"))
            .sum();
        assert!(total <= info.emission_amount);
    }

    #[test]
    fn unspent_by_token_unknown_is_200_not_404() {
        assert_eq!(status("unspent_by_token_unknown"), 200);
        let boxes: Vec<IndexedBox> = load("unspent_by_token_unknown");
        assert!(boxes.is_empty());
    }

    #[test]
    fn box_by_id_spent() {
        let _: IndexedBox = load("box_by_id_spent");
        let raw: Value = load("box_by_id_spent");
        assert!(raw["spentTransactionId"].is_string());
        assert!(raw.get("spendingHeight").is_none(), "node now sends spendingHeight");
    }

    #[test]
    fn box_by_id_unknown() {
        assert_not_found("box_by_id_unknown");
    }

    #[test]
    fn with_pool_omits_missing_ids() {
        let requested = manifest()["fixtures"]["with_pool_partial"]["body"].clone();
        let existing = requested[1].as_str().expect("second id exists").to_string();

        let boxes: Vec<UTxO> = load("with_pool_partial");
        assert_eq!(boxes.len(), 1);
        assert_eq!(boxes[0].id.to_string(), existing);

        assert_eq!(status("with_pool_none"), 200);
        let none: Vec<UTxO> = load("with_pool_none");
        assert!(none.is_empty());
    }
}

mod source {
    use super::*;

    async fn node() -> (MockServer, NodeClient) {
        let server = MockServer::start().await;
        let client = NodeClient::new(reqwest::Client::new(), &server.uri(), Network::Mainnet);
        (server, client)
    }

    /// Replies with a fixture and the status it was recorded with.
    fn recorded(name: &str) -> ResponseTemplate {
        ResponseTemplate::new(status(name)).set_body_raw(fixtures::text(name), "application/json")
    }

    fn json(status: u16, body: Value) -> ResponseTemplate {
        ResponseTemplate::new(status).set_body_json(body)
    }

    fn not_found() -> ResponseTemplate {
        recorded("token_unknown")
    }

    fn page(offset: u32, limit: u32) -> Page {
        Page { offset, limit }
    }

    #[tokio::test]
    async fn connect_reads_the_network() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/info"))
            .respond_with(recorded("info"))
            .expect(1)
            .mount(&server)
            .await;
        let client = NodeClient::connect(reqwest::Client::new(), &server.uri())
            .await
            .unwrap();
        assert_eq!(client.network(), Network::Mainnet);
    }

    #[tokio::test]
    async fn connect_rejects_an_unknown_network() {
        let server = MockServer::start().await;
        Mock::given(path("/info"))
            .respond_with(json(
                200,
                serde_json::json!({"lastMemPoolUpdateTime": 1, "network": "devnet"}),
            ))
            .mount(&server)
            .await;
        let result = NodeClient::connect(reqwest::Client::new(), &server.uri()).await;
        assert!(matches!(result, Err(NodeError::UnknownNetwork(_))));
    }

    #[tokio::test]
    async fn headers_and_indexed_height() {
        let (server, node) = node().await;
        Mock::given(method("GET"))
            .and(path("/blocks/lastHeaders/3"))
            .respond_with(recorded("last_headers"))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/blockchain/indexedHeight"))
            .respond_with(recorded("indexed_height"))
            .expect(1)
            .mount(&server)
            .await;

        assert_eq!(node.last_headers(3).await.unwrap().len(), 3);
        let height = node.indexed_height().await.unwrap();
        assert_eq!(height.indexed_height, height.full_height);
    }

    #[tokio::test]
    async fn indexed_block_or_none() {
        let (server, node) = node().await;
        let id = digest(&sample("headerId"));
        let unknown = digest(&"ab".repeat(32));
        Mock::given(method("GET"))
            .and(path(format!("/blockchain/block/byHeaderId/{id}")))
            .respond_with(recorded("indexed_block"))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(format!("/blockchain/block/byHeaderId/{unknown}")))
            .respond_with(recorded("indexed_block_unknown"))
            .expect(1)
            .mount(&server)
            .await;

        assert_eq!(node.indexed_block(&id).await.unwrap().unwrap().header.id, id);
        assert_eq!(node.indexed_block(&unknown).await.unwrap(), None);
    }

    #[tokio::test]
    async fn mempool_drops_unresolved_transactions() {
        let (server, node) = node().await;
        Mock::given(method("GET"))
            .and(path("/transactions/unconfirmed"))
            .respond_with(recorded("mempool_unresolved_input"))
            .expect(1)
            .mount(&server)
            .await;

        let all: Vec<Value> = load("mempool_unresolved_input");
        let txs = node.mempool().await.unwrap();
        assert_eq!(txs.len(), all.len() - 1);
        assert!(txs.iter().all(|tx| !tx.inputs.is_empty()));
    }

    #[tokio::test]
    async fn unspent_by_ergo_tree_posts_the_tree_and_pages_oldest_first() {
        let (server, node) = node().await;
        let tree = sample_tree();
        Mock::given(method("POST"))
            .and(path("/blockchain/box/unspent/byErgoTree"))
            .and(body_json(sample("ergoTree")))
            .and(query_param("offset", "10"))
            .and(query_param("limit", "5"))
            .and(query_param("sortDirection", "asc"))
            .respond_with(recorded("unspent_by_ergo_tree"))
            .expect(1)
            .mount(&server)
            .await;

        let boxes = node.unspent_by_ergo_tree(&tree, page(10, 5)).await.unwrap();
        assert!(!boxes.is_empty());
        assert!(boxes.iter().all(|b| b.utxo.ergo_tree == tree));
    }

    #[tokio::test]
    async fn unspent_empty_or_404_is_an_empty_set() {
        let (server, node) = node().await;
        let empty = HexBytes(vec![0x01]);
        let missing = HexBytes(vec![0x02]);
        let token = digest(&"ab".repeat(32));
        Mock::given(body_json("01"))
            .respond_with(recorded("unspent_by_ergo_tree_empty"))
            .mount(&server)
            .await;
        Mock::given(body_json("02"))
            .respond_with(not_found())
            .mount(&server)
            .await;
        Mock::given(path(format!("/blockchain/box/unspent/byTokenId/{token}")))
            .respond_with(not_found())
            .mount(&server)
            .await;

        assert!(
            node.unspent_by_ergo_tree(&empty, page(0, 5))
                .await
                .unwrap()
                .is_empty()
        );
        assert!(
            node.unspent_by_ergo_tree(&missing, page(0, 5))
                .await
                .unwrap()
                .is_empty()
        );
        assert!(
            node.unspent_by_token(&token, page(0, 5))
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn unspent_by_token() {
        let (server, node) = node().await;
        let token = digest(&sample("smallSupplyToken"));
        Mock::given(method("GET"))
            .and(path(format!("/blockchain/box/unspent/byTokenId/{token}")))
            .and(query_param("offset", "0"))
            .and(query_param("limit", "100"))
            .and(query_param("sortDirection", "asc"))
            .respond_with(recorded("unspent_by_token_small_supply"))
            .expect(1)
            .mount(&server)
            .await;

        let boxes = node.unspent_by_token(&token, page(0, 100)).await.unwrap();
        assert!(boxes.len() >= 2);
    }

    #[tokio::test]
    async fn token_info_or_none() {
        let (server, node) = node().await;
        let nft = digest(&sample("nft"));
        let unknown = digest(&"ab".repeat(32));
        Mock::given(path(format!("/blockchain/token/byId/{nft}")))
            .respond_with(recorded("token_nft"))
            .mount(&server)
            .await;
        Mock::given(path(format!("/blockchain/token/byId/{unknown}")))
            .respond_with(recorded("token_unknown"))
            .mount(&server)
            .await;

        assert_eq!(
            node.token_info(&nft)
                .await
                .unwrap()
                .unwrap()
                .emission_amount,
            1
        );
        assert_eq!(node.token_info(&unknown).await.unwrap(), None);
    }

    #[tokio::test]
    async fn boxes_with_pool_posts_ids() {
        let (server, node) = node().await;
        let requested = manifest()["fixtures"]["with_pool_partial"]["body"].clone();
        let ids: Vec<BoxId> = serde_json::from_value(requested.clone()).unwrap();
        Mock::given(method("POST"))
            .and(path("/utxo/withPool/byIds"))
            .and(body_json(requested))
            .respond_with(recorded("with_pool_partial"))
            .expect(1)
            .mount(&server)
            .await;

        let boxes = node.boxes_with_pool(&ids).await.unwrap();
        assert_eq!(boxes.len(), 1);
        assert_eq!(boxes[0].id, ids[1]);
    }

    #[tokio::test]
    async fn indexed_box_or_none() {
        let (server, node) = node().await;
        let spent = manifest()["fixtures"]["box_by_id_spent"]["path"]
            .as_str()
            .unwrap()
            .rsplit('/')
            .next()
            .map(digest)
            .unwrap();
        let unknown = digest(&"ab".repeat(32));
        Mock::given(path(format!("/blockchain/box/byId/{spent}")))
            .respond_with(recorded("box_by_id_spent"))
            .mount(&server)
            .await;
        Mock::given(path(format!("/blockchain/box/byId/{unknown}")))
            .respond_with(recorded("box_by_id_unknown"))
            .mount(&server)
            .await;

        assert_eq!(node.indexed_box(&spent).await.unwrap().unwrap().utxo.id, spent);
        assert_eq!(node.indexed_box(&unknown).await.unwrap(), None);
    }

    #[tokio::test]
    async fn confirmed_balance_posts_the_trees_address() {
        let (server, node) = node().await;
        Mock::given(method("POST"))
            .and(path("/blockchain/balance"))
            .and(body_json(sample("address")))
            .respond_with(recorded("balance"))
            .expect(1)
            .mount(&server)
            .await;

        let balance = node.confirmed_balance(&sample_tree()).await.unwrap();
        assert!(balance.nano_ergs > 0);
    }

    #[tokio::test]
    async fn unavailable_carries_retry_after() {
        let (server, node) = node().await;
        Mock::given(path("/blockchain/indexedHeight"))
            .respond_with(ResponseTemplate::new(503).insert_header("Retry-After", "2"))
            .mount(&server)
            .await;
        Mock::given(path("/blockchain/token/byId/".to_string() + &"ab".repeat(32)))
            .respond_with(ResponseTemplate::new(503))
            .mount(&server)
            .await;

        assert!(matches!(
            node.indexed_height().await,
            Err(NodeError::Unavailable { retry_after: Some(d) }) if d == Duration::from_secs(2)
        ));
        assert!(matches!(
            node.token_info(&digest(&"ab".repeat(32))).await,
            Err(NodeError::Unavailable { retry_after: None })
        ));
    }

    #[tokio::test]
    async fn other_errors_carry_the_reason() {
        let (server, node) = node().await;
        let body = serde_json::json!({
            "error": 400,
            "reason": "bad.request",
            "detail": "No more than 16384 boxes can be requested"
        });
        Mock::given(path("/blockchain/box/unspent/byErgoTree"))
            .respond_with(json(400, body))
            .mount(&server)
            .await;
        Mock::given(path("/info"))
            .respond_with(ResponseTemplate::new(500).set_body_string("boom"))
            .mount(&server)
            .await;

        let err = node
            .unspent_by_ergo_tree(&sample_tree(), page(0, 20000))
            .await
            .unwrap_err();
        let NodeError::Api { status, reason, detail } = err else { panic!("{err:?}") };
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(reason, "bad.request");
        assert_eq!(detail.as_deref(), Some("No more than 16384 boxes can be requested"));

        let err = node.info().await.unwrap_err();
        assert!(matches!(err, NodeError::Api { reason, .. } if reason == "boom"));
    }
}
