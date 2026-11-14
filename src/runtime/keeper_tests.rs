//! The keeper against `SimChain` and a real engine task, under a paused clock.

use std::sync::Arc;

use tokio::task::JoinHandle;

use super::*;
use crate::{
    adapters::{BlockConsumer, EngineGone, EngineHandle, loader},
    engine::{self, Engine, Query, Reply},
    runtime::spawn,
    sim::{Chain, SimChain, SimConfig, token_id},
};

const CONFIG: engine::Config = engine::Config { window: 4, grace: 2 };
const RETRY: Duration = Duration::from_secs(5);

/// A chain mined a little, with an indexer that recovers from a reorg at once, and an engine
/// task synced to its tip.
async fn synced() -> (SimChain, Handle, JoinHandle<()>) {
    let mut chain = Chain::new(SimConfig { recovery_reads: 0, ..SimConfig::default() });
    chain.mine(0, 2, 1);
    let sim = SimChain::new(chain);
    let (engine, task) = spawn(Engine::new(CONFIG));
    sync(&sim, &engine).await;
    (sim, engine, task)
}

async fn sync(sim: &SimChain, engine: &Handle) {
    let tip = sim.lock().tip();
    BlockConsumer::new(sim.clone())
        .sync(engine, tip)
        .await
        .unwrap();
}

fn keeper(sim: &SimChain, engine: &Handle) -> Arc<Keeper<SimChain>> {
    Arc::new(Keeper::new(
        engine.clone(),
        Loader::new(sim.clone(), loader::Config::default()),
        Arc::new(TokenChecker::new(sim.clone(), 100)),
        Config::default(),
    ))
}

fn keep(keeper: &Arc<Keeper<SimChain>>, key: &TrackingKey) -> JoinHandle<Result<(), EngineGone>> {
    let (keeper, key) = (keeper.clone(), key.clone());
    tokio::spawn(async move { keeper.keep(key).await })
}

/// The key's confirmed balance, if it is tracked. A read is activity for the grace period.
async fn confirmed(engine: &Handle, key: &TrackingKey) -> Option<crate::engine::Totals> {
    match engine.query(Query::Balance(key.clone())).await.unwrap() {
        Reply::Balance(balance) => balance.map(|b| b.confirmed),
        other => panic!("{other:?}"),
    }
}

/// Waits, in paused time, until the key is tracked at the node's tip.
async fn tracked_at_tip(sim: &SimChain, engine: &Handle, key: &TrackingKey) {
    for _ in 0..100 {
        let tip = sim.lock().tip();
        if let Reply::Balance(Some(balance)) =
            engine.query(Query::Balance(key.clone())).await.unwrap()
            && balance.as_of == tip
        {
            let expected = sim.lock().oracle().totals(key, tip.height);
            assert_eq!(balance.confirmed, expected, "{key:?}");
            return;
        }
        sleep(Duration::from_millis(100)).await;
    }
    panic!("{key:?} never tracked at the tip");
}

#[tokio::test(start_paused = true)]
async fn a_configured_key_outlives_the_grace_period() {
    let (sim, engine, _task) = synced().await;
    let keys = sim.lock().keys();
    let (kept, loaded) = (&keys[0], &keys[1]);
    let _keeper = keep(&keeper(&sim, &engine), kept);
    tracked_at_tip(&sim, &engine, kept).await;
    Loader::new(sim.clone(), loader::Config::default())
        .load(&engine, loaded)
        .await
        .unwrap();

    for seed in 0..CONFIG.grace + 2 {
        sim.lock().mine(0, 1, seed.into());
        sync(&sim, &engine).await;
    }
    assert!(confirmed(&engine, kept).await.is_some(), "the keeper's subscription holds it");
    assert!(confirmed(&engine, loaded).await.is_none(), "a key no one holds goes idle");
}

#[tokio::test(start_paused = true)]
async fn a_key_dropped_with_its_load_block_is_loaded_again() {
    let (sim, engine, _task) = synced().await;
    let key = TrackingKey::Token(token_id("small-100"));
    let keeper = keep(&keeper(&sim, &engine), &key);
    tracked_at_tip(&sim, &engine, &key).await;
    let loaded_at = sim.lock().tip();

    // The load block is orphaned: the engine drops the key, and the keeper loads it again
    // on the new branch.
    {
        let mut chain = sim.lock();
        chain.orphan(1);
        chain.mine(0, 1, 11);
        chain.mine(0, 1, 12);
    }
    sync(&sim, &engine).await;
    tracked_at_tip(&sim, &engine, &key).await;
    assert_ne!(sim.lock().tip(), loaded_at);
    assert!(!keeper.is_finished());
}

#[tokio::test(start_paused = true)]
async fn a_token_above_the_supply_limit_is_given_up_alone() {
    let (sim, engine, _task) = synced().await;
    let keeper = keeper(&sim, &engine);
    let large = keep(&keeper, &TrackingKey::Token(token_id("over-101")));
    let small = TrackingKey::Token(token_id("small-100"));
    let _kept = keep(&keeper, &small);
    let given_up = tokio::time::timeout(RETRY * 10, large)
        .await
        .expect("the keeper gives the key up")
        .unwrap();
    assert_eq!(given_up, Ok(()));
    tracked_at_tip(&sim, &engine, &small).await;
}

#[tokio::test(start_paused = true)]
async fn an_unknown_token_is_asked_again_and_kept_once_minted() {
    let (sim, engine, _task) = synced().await;
    let owner = sim.lock().trees()[0].clone();
    let input = crate::sim::testing::box_of(&sim.lock(), 0);
    let key = TrackingKey::Token(input);
    let keeper = keep(&keeper(&sim, &engine), &key);
    sleep(RETRY * 3).await;
    assert!(!keeper.is_finished(), "an unknown token is a wait");

    {
        let mut chain = sim.lock();
        let mint = chain.mint(&input, 3, &owner);
        chain.mine_txs(vec![mint]);
    }
    sync(&sim, &engine).await;
    tracked_at_tip(&sim, &engine, &key).await;
}

#[tokio::test(start_paused = true)]
async fn a_load_that_gave_up_is_tried_again_after_its_wait() {
    // The node is a block ahead of an engine that never catches up: every attempt reads
    // `/info` once and finds the engine behind.
    let (sim, engine, _task) = synced().await;
    sim.lock().mine(0, 1, 9);
    let key = sim.lock().keys()[0].clone();
    let loader = loader::Config::default();
    let _keeper = keep(&keeper(&sim, &engine), &key);
    let span = loader.retry * loader.attempts * 10;
    sleep(span).await;
    let reads = sim
        .calls()
        .iter()
        .filter(|e| **e == crate::sim::Endpoint::Info)
        .count();
    // One attempt per retry, with no burst when the loader gives up.
    let most = (span.as_secs() / loader.retry.as_secs()) as usize + 1;
    assert!(reads <= most, "{reads} reads in {span:?}, at most {most} expected");
}

#[tokio::test(start_paused = true)]
async fn the_keeper_stops_when_the_engine_is_gone() {
    let (sim, engine, task) = synced().await;
    task.abort();
    let key = sim.lock().keys()[0].clone();
    assert_eq!(keep(&keeper(&sim, &engine), &key).await.unwrap(), Err(EngineGone));
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
        engine::Event,
        fixtures::{self, load, sample},
        types::{HexBytes, address::Network, ergo::IndexedBlock},
    };

    /// The node serves the recorded block as its tip, and the recorded boxes of the sample
    /// tree for any tree.
    async fn node_with_wrong_boxes(block: &IndexedBlock) -> MockServer {
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
        Mock::given(method("POST"))
            .and(path("/blockchain/box/unspent/byErgoTree"))
            .respond_with(
                ResponseTemplate::new(fixtures::status("unspent_by_ergo_tree"))
                    .set_body_raw(fixtures::text("unspent_by_ergo_tree"), "application/json"),
            )
            .mount(&server)
            .await;
        server
    }

    /// On real time: a paused clock would jump ahead while the HTTP calls are in flight.
    #[tokio::test]
    async fn boxes_that_do_not_match_the_key_are_asked_again_later() {
        let block: IndexedBlock = load("indexed_block");
        let (engine, _task) = spawn(Engine::new(engine::Config::default()));
        engine
            .apply(Event::BlockApplied(block.clone()))
            .await
            .unwrap()
            .unwrap();
        let server = node_with_wrong_boxes(&block).await;
        let client = NodeClient::new(reqwest::Client::new(), &server.uri(), Network::Mainnet);
        let config =
            Config { retry: Duration::from_millis(10), mismatch_retry: Duration::from_millis(100) };
        let keeper = Arc::new(Keeper::new(
            engine.clone(),
            Loader::new(client.clone(), loader::Config::default()),
            Arc::new(TokenChecker::new(client, 100)),
            config,
        ));
        let mut other = hex::decode(sample("ergoTree")).unwrap();
        *other.last_mut().unwrap() ^= 1;
        let key = TrackingKey::Tree(HexBytes(other));
        let kept = tokio::spawn({
            let key = key.clone();
            async move { keeper.keep(key).await }
        });

        let loads = || async {
            let requests = server.received_requests().await.unwrap();
            let by_ergo_tree = "/blockchain/box/unspent/byErgoTree";
            requests
                .iter()
                .filter(|r| r.url.path() == by_ergo_tree)
                .count()
        };
        let started = tokio::time::Instant::now();
        while loads().await < 3 {
            assert!(started.elapsed() < Duration::from_secs(10), "the load is asked again");
            sleep(Duration::from_millis(20)).await;
        }
        assert!(
            started.elapsed() >= config.mismatch_retry * 2,
            "loads are spaced by the mismatch wait, not the short retry"
        );
        assert!(!kept.is_finished(), "a node fault never stops the keeper");
        let reply = engine.query(Query::Balance(key)).await.unwrap();
        assert_eq!(reply, Reply::Balance(None), "the mismatched boxes were not loaded");
    }
}
