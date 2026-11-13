//! The follower: one `/info` read per tick for blocks then mempool, the mempool skipped while
//! the engine is behind, back-off, and node failures (`500`, timeouts) through `wiremock`.

use std::sync::{Arc, Mutex};

use super::*;
use crate::{
    engine::{BlockRef, Config as EngineConfig, Engine, Query, Reply},
    sim::{
        Chain, Endpoint, SimChain, SimConfig,
        testing::{new_chain, ready},
    },
};

fn engine() -> Mutex<Engine> {
    Mutex::new(Engine::new(EngineConfig::default()))
}

/// The snapshot the engine uses, and the tip it is in use at.
fn engine_snapshot(engine: &Mutex<Engine>) -> Option<(usize, BlockRef)> {
    match engine.lock().unwrap().query(Query::MempoolSnapshot) {
        Reply::MempoolSnapshot(snapshot) => snapshot.map(|(txs, tip)| (txs.len(), tip)),
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_tick_applies_the_blocks_then_the_mempool_at_that_tip() {
    let mut chain = new_chain();
    chain.mempool_add(1).expect("a pending tx");
    let sim = SimChain::new(chain);
    let engine = engine();
    let mut follower = Follower::new(sim.clone(), Config::default());
    let tick = ready(follower.tick(&engine, Instant::now())).unwrap();
    assert!(matches!(tick.blocks, Ok(Sync::Synced { applied: 1, .. })), "{tick:?}");
    assert!(matches!(tick.mempool, Some(Ok(Poll::Snapshot { .. }))), "{tick:?}");
    use Endpoint::{IndexedBlock, IndexedHeight, Info, Mempool};
    assert_eq!(sim.calls(), [Info, IndexedHeight, IndexedBlock, Mempool, Info]);
    let tip = sim.lock().tip();
    assert_eq!(engine_snapshot(&engine), Some((1, tip)));
}

#[test]
fn the_mempool_waits_while_the_engine_is_behind() {
    let mut chain = Chain::new(SimConfig::default());
    chain.mine(0, 1, 1);
    let sim = SimChain::new(chain);
    let engine = engine();
    let mut follower = Follower::new(sim.clone(), Config::default());
    let now = Instant::now();
    ready(follower.tick(&engine, now)).unwrap();

    // A same-height reorg: the recovering indexer answers 503 to its next reads.
    sim.lock().orphan(1);
    sim.lock().mine(0, 2, 2);
    sim.lock().mempool_add(3).expect("a pending tx");
    for _ in 0..SimConfig::default().recovery_reads {
        let tick = ready(follower.tick(&engine, now)).unwrap();
        assert!(matches!(tick.blocks, Err(NodeError::Unavailable { .. })), "{tick:?}");
        assert!(tick.mempool.is_none());
    }
    let tick = ready(follower.tick(&engine, now)).unwrap();
    assert!(matches!(tick.blocks, Ok(Sync::Synced { reverted: 1, applied: 1, .. })), "{tick:?}");
    assert!(matches!(tick.mempool, Some(Ok(Poll::Snapshot { .. }))), "{tick:?}");
    let tip = sim.lock().tip();
    assert_eq!(engine_snapshot(&engine).map(|s| s.1), Some(tip));
}

#[tokio::test(start_paused = true)]
async fn run_follows_the_node() {
    let sim = SimChain::new(new_chain());
    let engine = Arc::new(engine());
    let follower = Follower::new(sim.clone(), Config::default());
    let task = {
        let engine = engine.clone();
        tokio::spawn(async move { follower.run(&*engine).await })
    };
    for seed in 1..=3 {
        sim.lock().mine(0, 1, seed);
        sim.lock().mempool_add(seed + 10);
        tokio::time::sleep(Duration::from_secs(2)).await;
        let tip = sim.lock().tip();
        assert_eq!(engine.lock().unwrap().tip(), Some(tip));
        let pool = sim.lock().mempool().len();
        assert_eq!(engine_snapshot(&engine), Some((pool, tip)));
    }
    task.abort();
}

mod node {
    use serde_json::{Value, json};
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{method, path},
    };

    use super::*;
    use crate::{
        clients::NodeClient,
        fixtures::{self, load},
        types::{address::Network, ergo::IndexedBlock},
    };

    fn recorded(name: &str) -> ResponseTemplate {
        ResponseTemplate::new(fixtures::status(name))
            .set_body_raw(fixtures::text(name), "application/json")
    }

    /// A node at the recorded block, whose first mempool answer is `first`.
    async fn node(first: ResponseTemplate) -> MockServer {
        let block: IndexedBlock = load("indexed_block");
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
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "indexedHeight": block.header.height,
                "fullHeight": block.header.height,
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(format!("/blockchain/block/byHeaderId/{}", block.header.id)))
            .respond_with(recorded("indexed_block"))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/transactions/unconfirmed"))
            .respond_with(first)
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/transactions/unconfirmed"))
            .respond_with(recorded("mempool"))
            .mount(&server)
            .await;
        server
    }

    async fn a_failed_mempool_fetch_is_retried_next_tick(
        first: ResponseTemplate,
        timeout: Duration,
    ) {
        let server = node(first).await;
        let http = reqwest::Client::builder().timeout(timeout).build().unwrap();
        let client = NodeClient::new(http, &server.uri(), Network::Mainnet);
        let engine = engine();
        let mut follower = Follower::new(client, Config::default());
        let now = Instant::now();

        let tick = follower.tick(&engine, now).await.unwrap();
        assert!(matches!(tick.blocks, Ok(Sync::Synced { applied: 1, .. })), "{tick:?}");
        assert!(matches!(tick.mempool, Some(Err(_))), "{tick:?}");
        assert_eq!(engine_snapshot(&engine), None);

        let tick = follower.tick(&engine, now).await.unwrap();
        assert!(matches!(tick.blocks, Ok(Sync::Idle)), "{tick:?}");
        assert!(matches!(tick.mempool, Some(Ok(Poll::Snapshot { .. }))), "{tick:?}");
        assert!(engine_snapshot(&engine).is_some());
    }

    #[tokio::test]
    async fn a_500_is_retried_next_tick() {
        a_failed_mempool_fetch_is_retried_next_tick(
            ResponseTemplate::new(500),
            Duration::from_secs(10),
        )
        .await;
    }

    #[tokio::test]
    async fn a_timeout_is_retried_next_tick() {
        a_failed_mempool_fetch_is_retried_next_tick(
            recorded("mempool").set_delay(Duration::from_millis(500)),
            Duration::from_millis(100),
        )
        .await;
    }

    #[tokio::test]
    async fn a_failed_info_read_is_an_error() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/info"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;
        let client = NodeClient::new(reqwest::Client::new(), &server.uri(), Network::Mainnet);
        let mut follower = Follower::new(client, Config::default());
        let error = follower.tick(&engine(), Instant::now()).await.unwrap_err();
        assert!(matches!(error, NodeError::Api { .. }), "{error:?}");
    }
}
