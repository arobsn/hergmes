//! The box fetcher against `SimChain` (each source in order, chunking, the cache) and against
//! `wiremock` (a partial `withPool/byIds` answer matched by id).

use std::sync::Mutex;

use super::*;
use crate::{
    engine::{Config, Engine, Event, TrackingKey},
    sim::{
        Chain, Endpoint, SimChain,
        testing::{box_of, new_chain, ready},
    },
};

fn capacity(n: usize) -> NonZeroUsize {
    NonZeroUsize::new(n).unwrap()
}

fn synced_engine(chain: &Chain) -> Mutex<Engine> {
    let mut engine = Engine::new(Config::default());
    engine
        .apply(Event::BlockApplied(chain.oracle().indexed_block(chain.height())))
        .unwrap();
    Mutex::new(engine)
}

fn calls(sim: &SimChain, endpoint: Endpoint) -> usize {
    sim.calls().iter().filter(|e| **e == endpoint).count()
}

fn ids(boxes: &[Option<Arc<UTxO>>]) -> Vec<Option<BoxId>> {
    boxes.iter().map(|b| b.as_ref().map(|b| b.id)).collect()
}

#[test]
fn each_box_comes_from_the_first_source_that_has_it() {
    let mut chain = new_chain();
    let engine = synced_engine(&chain);
    // Tree 0 is tracked; a block spends one of its boxes, so only the ring holds it.
    let key = TrackingKey::Tree(chain.trees()[0].clone());
    let as_of = chain.tip();
    let boxes = chain.oracle().load(&key, as_of.height);
    engine
        .lock()
        .unwrap()
        .apply(Event::KeyLoaded { key, as_of, boxes })
        .unwrap();
    let in_ring = box_of(&chain, 0);
    let tx = chain.transfer(&[in_ring], &chain.trees()[4].clone());
    let block = chain.mine_txs(vec![tx]);
    engine
        .lock()
        .unwrap()
        .apply(Event::BlockApplied(chain.oracle().indexed_block(block.height)))
        .unwrap();
    // An unspent box, and a box a block the engine has not seen spent (untracked, so not in
    // the ring either): only the indexer still has it.
    let unspent = box_of(&chain, 1);
    let spent_elsewhere = box_of(&chain, 2);
    let tx = chain.transfer(&[spent_elsewhere], &chain.trees()[3].clone());
    chain.mine_txs(vec![tx]);
    let pending = chain.mempool_add(3).expect("a pending tx").outputs[0].id;
    let unknown = crate::sim::testing::digest(250);

    let sim = SimChain::new(chain);
    let fetcher = BoxFetcher::new(sim.clone(), capacity(100));
    let wanted = [in_ring, unspent, spent_elsewhere, pending, unknown, unspent];
    let boxes = ready(fetcher.fetch(&engine, &wanted)).unwrap();
    assert_eq!(
        ids(&boxes),
        [Some(in_ring), Some(unspent), Some(spent_elsewhere), Some(pending), None, Some(unspent)]
    );
    for b in boxes.iter().flatten() {
        assert_eq!(Some(b.clone()), sim.lock().find_box(&b.id));
    }
    // The ring answered first; the node's set and pool got the rest in one call, without the
    // repeated id; the indexer got what was left.
    let (with_pool, indexed) =
        (calls(&sim, Endpoint::BoxesWithPool), calls(&sim, Endpoint::IndexedBox));
    assert_eq!((with_pool, indexed), (1, 2), "the spent box and the unknown id");

    // Everything found is cached: no node call the second time.
    let before = sim.calls().len();
    let again =
        ready(fetcher.fetch(&engine, &[in_ring, unspent, spent_elsewhere, pending])).unwrap();
    assert!(again.iter().all(Option::is_some));
    assert_eq!(sim.calls().len(), before);
}

#[test]
fn large_id_lists_go_in_chunks() {
    let chain = new_chain();
    let engine = synced_engine(&chain);
    let wanted: Vec<BoxId> = chain.utxo().values().map(|b| b.id).take(5).collect();
    let sim = SimChain::new(chain);
    let fetcher = BoxFetcher::with_chunk(sim.clone(), capacity(100), 2);
    let boxes = ready(fetcher.fetch(&engine, &wanted)).unwrap();
    assert!(boxes.iter().all(Option::is_some));
    assert_eq!(calls(&sim, Endpoint::BoxesWithPool), 3);
}

#[test]
fn the_cache_evicts_the_least_recently_used_box() {
    let chain = new_chain();
    let engine = synced_engine(&chain);
    let (a, b) = (box_of(&chain, 0), box_of(&chain, 1));
    let sim = SimChain::new(chain);
    let fetcher = BoxFetcher::new(sim.clone(), capacity(1));
    ready(fetcher.fetch(&engine, &[a])).unwrap();
    ready(fetcher.fetch(&engine, &[a])).unwrap();
    assert_eq!(calls(&sim, Endpoint::BoxesWithPool), 1);
    ready(fetcher.fetch(&engine, &[b])).unwrap();
    ready(fetcher.fetch(&engine, &[a])).unwrap();
    assert_eq!(calls(&sim, Endpoint::BoxesWithPool), 3, "b evicted a");
}

mod node {
    use serde_json::Value;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{method, path},
    };

    use super::*;
    use crate::{
        clients::NodeClient,
        fixtures::{self, load},
        types::address::Network,
    };

    fn recorded(name: &str) -> ResponseTemplate {
        ResponseTemplate::new(fixtures::status(name))
            .set_body_raw(fixtures::text(name), "application/json")
    }

    #[tokio::test]
    async fn a_partial_with_pool_answer_is_matched_by_id() {
        let found: Vec<Value> = load("with_pool_partial");
        let present: BoxId = found[0]["boxId"].as_str().unwrap().parse().unwrap();
        let spent: Value = load("box_by_id_spent");
        let spent_id: BoxId = spent["boxId"].as_str().unwrap().parse().unwrap();
        let unknown = crate::sim::testing::digest(250);

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/utxo/withPool/byIds"))
            .respond_with(recorded("with_pool_partial"))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(format!("/blockchain/box/byId/{spent_id}")))
            .respond_with(recorded("box_by_id_spent"))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(format!("/blockchain/box/byId/{unknown}")))
            .respond_with(recorded("box_by_id_unknown"))
            .expect(1)
            .mount(&server)
            .await;

        let client = NodeClient::new(reqwest::Client::new(), &server.uri(), Network::Mainnet);
        let fetcher = BoxFetcher::new(client, capacity(10));
        let engine = Mutex::new(Engine::new(Config::default()));
        // The found box is asked last, so a positional match would be wrong.
        let boxes = fetcher
            .fetch(&engine, &[spent_id, unknown, present])
            .await
            .unwrap();
        assert_eq!(ids(&boxes), [Some(spent_id), None, Some(present)]);
    }
}
