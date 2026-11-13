//! The loader against `SimChain` (pinning, paging, the waits, a block between pages, a
//! recovering indexer, a rejection, the retry loop) and against `wiremock` (a box that does
//! not match the key).

use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};

use super::*;
use crate::{
    adapters::{Follower, follower},
    engine::{Config as EngineConfig, Engine, EngineError, Query, Reply},
    sim::{
        Chain, Endpoint, SimChain, SimConfig,
        testing::{new_chain, ready},
    },
};

fn synced_engine(chain: &Chain) -> Mutex<Engine> {
    let mut engine = Engine::new(EngineConfig::default());
    engine
        .apply(Event::BlockApplied(chain.oracle().indexed_block(chain.height())))
        .unwrap();
    Mutex::new(engine)
}

/// The key's boxes in the engine, and the oracle's at the node's tip.
fn engine_and_oracle(
    sim: &SimChain,
    engine: &Mutex<Engine>,
    key: &TrackingKey,
) -> (Vec<crate::engine::BoxKey>, Vec<crate::engine::BoxKey>) {
    let tip = sim.lock().tip();
    let expected = sim
        .lock()
        .oracle()
        .unspent(key, tip.height)
        .into_keys()
        .collect();
    let query = Query::Page { key: key.clone(), at: tip, after: None, limit: usize::MAX };
    match engine.lock().unwrap().query(query) {
        Reply::Page(Ok(boxes)) => (boxes, expected),
        other => panic!("{other:?}"),
    }
}

fn tree_key(sim: &SimChain, tree: usize) -> TrackingKey {
    TrackingKey::Tree(sim.lock().trees()[tree].clone())
}

fn count(sim: &SimChain, endpoint: Endpoint) -> usize {
    sim.calls().iter().filter(|e| **e == endpoint).count()
}

#[test]
fn a_key_is_loaded_at_the_tip() {
    let chain = new_chain();
    let engine = synced_engine(&chain);
    let sim = SimChain::new(chain);
    let key = tree_key(&sim, 0);
    let loader = Loader::new(sim.clone(), Config::default());
    let tip = sim.lock().tip();
    let outcome = ready(loader.try_load(&engine, &key)).unwrap();
    let (boxes, expected) = engine_and_oracle(&sim, &engine, &key);
    assert_eq!(outcome, Attempt::Tracked { as_of: tip, boxes: expected.len() });
    assert_eq!(boxes, expected);
    use Endpoint::{IndexedHeight, Info, UnspentByErgoTree};
    assert_eq!(sim.calls(), [Info, IndexedHeight, UnspentByErgoTree, Info]);
}

#[test]
fn pages_are_read_until_a_short_one() {
    // A token split over four boxes. One box per page ends with an empty page; three per
    // page ends with a page of one.
    for (page_limit, reads) in [(1, 5), (3, 2)] {
        let chain = new_chain();
        let engine = synced_engine(&chain);
        let sim = SimChain::new(chain);
        let key = TrackingKey::Token(crate::sim::token_id("small-100"));
        let loader = Loader::new(sim.clone(), Config { page_limit, ..Config::default() });
        let outcome = ready(loader.try_load(&engine, &key)).unwrap();
        assert!(matches!(outcome, Attempt::Tracked { boxes: 4, .. }), "{outcome:?}");
        assert_eq!(count(&sim, Endpoint::UnspentByToken), reads, "{page_limit} per page");
        let (boxes, expected) = engine_and_oracle(&sim, &engine, &key);
        assert_eq!(boxes, expected);
    }
}

#[test]
fn an_engine_behind_the_node_is_not_loaded_for() {
    let chain = new_chain();
    let engine = synced_engine(&chain);
    let sim = SimChain::new(chain);
    let before = engine.lock().unwrap().tip();
    let node = sim.lock().mine(0, 1, 1);
    let key = tree_key(&sim, 0);
    let loader = Loader::new(sim.clone(), Config::default());
    assert_eq!(
        ready(loader.try_load(&engine, &key)).unwrap(),
        Attempt::EngineBehind { engine: before, node }
    );
    assert_eq!(count(&sim, Endpoint::UnspentByErgoTree), 0);
}

#[test]
fn an_indexer_behind_the_tip_is_waited_for() {
    let mut chain = new_chain();
    chain.set_indexer_lag(1);
    let mut engine = Engine::new(EngineConfig::default());
    // The engine took the tip's block from elsewhere; the indexer has not reached it.
    let tip = chain.mine(0, 1, 1);
    engine
        .apply(Event::BlockApplied(chain.oracle().indexed_block(tip.height)))
        .unwrap();
    let engine = Mutex::new(engine);
    let sim = SimChain::new(chain);
    let key = tree_key(&sim, 0);
    let loader = Loader::new(sim.clone(), Config::default());
    assert_eq!(
        ready(loader.try_load(&engine, &key)).unwrap(),
        Attempt::IndexerBehind { indexed_height: tip.height - 1, tip_height: tip.height }
    );
    assert_eq!(count(&sim, Endpoint::UnspentByErgoTree), 0);
}

#[test]
fn a_block_between_pages_drops_the_load() {
    let chain = new_chain();
    let engine = synced_engine(&chain);
    let sim = SimChain::new(chain);
    let key = TrackingKey::Token(crate::sim::token_id("small-100"));
    let mut reads = 0;
    sim.on_call(move |endpoint, chain| {
        if endpoint == Endpoint::UnspentByToken {
            reads += 1;
            if reads == 2 {
                chain.mine(0, 1, 1);
            }
        }
    });
    let loader = Loader::new(sim.clone(), Config { page_limit: 1, ..Config::default() });
    assert_eq!(ready(loader.try_load(&engine, &key)).unwrap(), Attempt::TipMoved);
    assert!(!engine.lock().unwrap().is_tracked(&key));
}

#[test]
fn a_recovering_indexer_is_a_wait() {
    let mut chain = Chain::new(SimConfig::default());
    chain.mine(0, 1, 1);
    // A same-height reorg orphans the indexer's tip; the engine follows it by hand.
    chain.orphan(1);
    let tip = chain.mine(0, 2, 2);
    let mut engine = Engine::new(EngineConfig::default());
    engine
        .apply(Event::BlockApplied(chain.oracle().indexed_block(tip.height)))
        .unwrap();
    let engine = Mutex::new(engine);
    let sim = SimChain::new(chain);
    let key = tree_key(&sim, 1);
    let loader = Loader::new(sim.clone(), Config::default());
    let error = ready(loader.try_load(&engine, &key)).unwrap_err();
    assert!(
        matches!(error, LoadError::Node(NodeError::Unavailable { retry_after: Some(_) })),
        "{error:?}"
    );
    assert!(!engine.lock().unwrap().is_tracked(&key));
}

/// An engine that reverts its tip just before a load reaches it, as if a block event won the
/// race to the inbox.
struct Racing {
    inner: Mutex<Engine>,
    armed: AtomicBool,
}

impl EngineHandle for Racing {
    async fn apply(&self, event: Event) -> Result<Vec<Output>, EngineError> {
        let mut engine = self.inner.lock().unwrap();
        if matches!(event, Event::KeyLoaded { .. }) && self.armed.swap(false, Ordering::SeqCst) {
            let tip = engine.tip().expect("a tip");
            engine.apply(Event::BlockReverted(tip))?;
        }
        engine.apply(event)
    }

    async fn query(&self, query: Query) -> Reply {
        self.inner.lock().unwrap().query(query)
    }
}

#[test]
fn a_load_the_engine_rejects_is_reported() {
    let mut chain = new_chain();
    chain.mine(0, 1, 1);
    let mut engine = Engine::new(EngineConfig::default());
    for height in chain.height() - 1..=chain.height() {
        engine
            .apply(Event::BlockApplied(chain.oracle().indexed_block(height)))
            .unwrap();
    }
    let engine = Racing { inner: Mutex::new(engine), armed: AtomicBool::new(true) };
    let sim = SimChain::new(chain);
    let key = tree_key(&sim, 0);
    let loader = Loader::new(sim.clone(), Config::default());
    assert_eq!(ready(loader.try_load(&engine, &key)).unwrap(), Attempt::Rejected);
    assert!(!engine.inner.lock().unwrap().is_tracked(&key));
}

#[tokio::test(start_paused = true)]
async fn load_retries_until_the_engine_catches_up() {
    let chain = new_chain();
    let engine = Arc::new(synced_engine(&chain));
    let sim = SimChain::new(chain);
    sim.lock().mine(1, 1, 1);
    let key = tree_key(&sim, 0);

    let follower_engine = engine.clone();
    let follower = Follower::new(sim.clone(), follower::Config::default());
    // The follower's first tick runs at once; the load's first attempt runs before it.
    let loader = Loader::new(sim.clone(), Config::default());
    let first = loader.try_load(&*engine, &key).await.unwrap();
    assert!(matches!(first, Attempt::EngineBehind { .. }), "{first:?}");
    let task = tokio::spawn(async move { follower.run(&*follower_engine).await });

    let (as_of, boxes) = loader.load(&*engine, &key).await.unwrap();
    assert_eq!(as_of, sim.lock().tip());
    assert_eq!(boxes, engine_and_oracle(&sim, &engine, &key).1.len());
    task.abort();
}

#[tokio::test(start_paused = true)]
async fn load_gives_up_after_its_attempts() {
    let chain = new_chain();
    let engine = synced_engine(&chain);
    let sim = SimChain::new(chain);
    let node = sim.lock().mine(0, 1, 1);
    let key = tree_key(&sim, 0);
    let loader = Loader::new(sim.clone(), Config { attempts: 3, ..Config::default() });
    let error = loader.load(&engine, &key).await.unwrap_err();
    let before = engine.lock().unwrap().tip();
    assert!(
        matches!(
            error,
            LoadError::GaveUp { attempts: 3, last: Attempt::EngineBehind { engine, node: n } }
                if engine == before && n == node
        ),
        "{error:?}"
    );
    assert_eq!(count(&sim, Endpoint::Info), 3);
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
        fixtures::{self, load, sample},
        types::{HexBytes, address::Network, ergo::IndexedBlock},
    };

    fn recorded(name: &str) -> ResponseTemplate {
        ResponseTemplate::new(fixtures::status(name))
            .set_body_raw(fixtures::text(name), "application/json")
    }

    #[tokio::test]
    async fn a_box_that_does_not_match_the_key_is_refused() {
        // The engine is at the recorded block, and the node reports it as its tip.
        let block: IndexedBlock = load("indexed_block");
        let mut engine = Engine::new(EngineConfig::default());
        engine.apply(Event::BlockApplied(block.clone())).unwrap();
        let engine = Mutex::new(engine);

        let mut info: Value = load("info");
        info["bestFullHeaderId"] = block.header.id.to_string().into();
        info["fullHeight"] = block.header.height.into();
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/info"))
            .respond_with(ResponseTemplate::new(200).set_body_json(info))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/blockchain/indexedHeight"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "indexedHeight": block.header.height,
                "fullHeight": block.header.height,
            })))
            .mount(&server)
            .await;
        // The recorded boxes of the sample tree, served for another tree.
        Mock::given(method("POST"))
            .and(path("/blockchain/box/unspent/byErgoTree"))
            .respond_with(recorded("unspent_by_ergo_tree"))
            .mount(&server)
            .await;

        let client = NodeClient::new(reqwest::Client::new(), &server.uri(), Network::Mainnet);
        let loader = Loader::new(client, Config::default());
        let mut other = hex::decode(sample("ergoTree")).unwrap();
        *other.last_mut().unwrap() ^= 1;
        let key = TrackingKey::Tree(HexBytes(other));
        let error = loader.try_load(&engine, &key).await.unwrap_err();
        assert!(matches!(error, LoadError::Mismatch { .. }), "{error:?}");
        assert!(!engine.lock().unwrap().is_tracked(&key));
    }
}
