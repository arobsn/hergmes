//! The block consumer against `SimChain`, step by step: starting, catching up, reorgs at,
//! below and beyond the engine's chain, plain reverts, a lagging or recovering indexer, and a
//! block orphaned mid-walk.

use std::sync::Mutex;

use super::*;
use crate::{
    engine::Output,
    engine::{Config, Engine, Query, Reply, TrackingKey},
    sim::{
        Chain, Endpoint, SimChain, SimConfig,
        testing::{new_chain, ready},
    },
};

struct Fixture {
    sim: SimChain,
    engine: Mutex<Engine>,
    consumer: BlockConsumer<SimChain>,
}

impl Fixture {
    fn new(chain: Chain, window: u32) -> Self {
        let sim = SimChain::new(chain);
        Self {
            consumer: BlockConsumer::new(sim.clone()),
            engine: Mutex::new(Engine::new(Config { window, grace: 100 })),
            sim,
        }
    }

    fn sync(&self) -> Result<Sync, NodeError> {
        let tip = self.sim.lock().tip();
        ready(self.consumer.sync(&self.engine, tip))
    }

    fn engine_tip(&self) -> Option<BlockRef> {
        self.engine.lock().unwrap().tip()
    }

    fn chain_len(&self) -> usize {
        match self.engine.lock().unwrap().query(Query::Chain) {
            Reply::Chain(links) => links.len(),
            other => panic!("{other:?}"),
        }
    }

    fn key(&self, tree: usize) -> TrackingKey {
        TrackingKey::Tree(self.sim.lock().trees()[tree].clone())
    }

    /// Loads `key` at the node's tip, which must be the engine's.
    fn load(&self, key: &TrackingKey) {
        let (as_of, boxes) = {
            let chain = self.sim.lock();
            (chain.tip(), chain.oracle().load(key, chain.height()))
        };
        assert_eq!(self.engine_tip(), Some(as_of));
        let outputs = self
            .engine
            .lock()
            .unwrap()
            .apply(Event::KeyLoaded { key: key.clone(), as_of, boxes })
            .unwrap();
        assert!(
            !outputs
                .iter()
                .any(|o| matches!(o, Output::LoadRejected { .. }))
        );
    }

    /// The key's boxes in the engine equal the oracle's at the node's tip.
    fn assert_key_follows_the_oracle(&self, key: &TrackingKey) {
        let tip = self.sim.lock().tip();
        let expected: Vec<_> = self
            .sim
            .lock()
            .oracle()
            .unspent(key, tip.height)
            .into_keys()
            .collect();
        let query = Query::Page { key: key.clone(), at: tip, after: None, limit: usize::MAX };
        match self.engine.lock().unwrap().query(query) {
            Reply::Page(Ok(boxes)) => assert_eq!(boxes, expected),
            other => panic!("{other:?}"),
        }
    }

    fn calls(&self, endpoint: Endpoint) -> usize {
        self.sim.calls().iter().filter(|e| **e == endpoint).count()
    }
}

/// An indexer that follows reorgs at once. The simulator's recovering indexer moves on only
/// as its gated endpoints are read, which a consumer waiting on `indexedHeight` never does;
/// the real node recovers over time. `a_recovering_indexer_sends_nothing_until_it_answers`
/// covers the recovery.
fn instant_recovery() -> SimConfig {
    SimConfig { recovery_reads: 0, ..SimConfig::default() }
}

/// A chain a few blocks long, with spends between the trees.
fn grown_chain(config: SimConfig) -> Chain {
    let mut chain = Chain::new(config);
    for seed in 1..=4 {
        chain.mine(0, 2, seed);
    }
    chain
}

#[test]
fn the_first_sync_applies_the_tip_alone() {
    let f = Fixture::new(grown_chain(SimConfig::default()), 4);
    let tip = f.sim.lock().tip();
    assert_eq!(f.sync().unwrap(), Sync::Synced { reverted: 0, applied: 1, reset: false });
    assert_eq!(f.engine_tip(), Some(tip));
    assert_eq!(f.calls(Endpoint::IndexedBlock), 1);
    assert_eq!(f.sync().unwrap(), Sync::Idle);
    assert_eq!(f.sim.calls().len(), 2, "an idle sync makes no node call");
}

#[test]
fn new_blocks_are_walked_back_and_applied_oldest_first() {
    let f = Fixture::new(new_chain(), 4);
    f.sync().unwrap();
    let key = f.key(0);
    f.load(&key);
    for seed in 1..=3 {
        f.sim.lock().mine(1, 2, seed);
    }
    assert_eq!(f.sync().unwrap(), Sync::Synced { reverted: 0, applied: 3, reset: false });
    assert_eq!(f.engine_tip(), Some(f.sim.lock().tip()));
    assert_eq!(f.calls(Endpoint::IndexedBlock), 1 + 3);
    f.assert_key_follows_the_oracle(&key);
}

#[test]
fn a_reorg_reverts_to_the_fork_point_then_applies_the_branch() {
    let f = Fixture::new(Chain::new(instant_recovery()), 6);
    f.sync().unwrap();
    let key = f.key(1);
    f.load(&key);
    for seed in 1..=3 {
        f.sim.lock().mine(1, 2, seed);
    }
    f.sync().unwrap();
    assert_eq!(f.sim.lock().reorg(2, 0, 9), 2);
    assert_eq!(f.sync().unwrap(), Sync::Synced { reverted: 2, applied: 3, reset: false });
    assert_eq!(f.engine_tip(), Some(f.sim.lock().tip()));
    f.assert_key_follows_the_oracle(&key);
}

#[test]
fn going_back_to_a_block_the_engine_has_fetches_nothing() {
    let f = Fixture::new(new_chain(), 4);
    f.sync().unwrap();
    for seed in 1..=2 {
        f.sim.lock().mine(0, 1, seed);
    }
    f.sync().unwrap();
    let before = f.sim.calls().len();
    assert_eq!(f.sim.lock().orphan(2), 2);
    assert_eq!(f.sync().unwrap(), Sync::Synced { reverted: 2, applied: 0, reset: false });
    assert_eq!(f.engine_tip(), Some(f.sim.lock().tip()));
    assert_eq!(f.sim.calls().len(), before);
}

#[test]
fn a_lagging_indexer_is_waited_for() {
    let f = Fixture::new(new_chain(), 4);
    f.sync().unwrap();
    let before = f.engine_tip();
    f.sim.lock().set_indexer_lag(1);
    f.sim.lock().mine(0, 1, 1);
    let tip = f.sim.lock().tip();
    assert_eq!(
        f.sync().unwrap(),
        Sync::WaitingForIndexer { indexed_height: tip.height - 1, tip_height: tip.height }
    );
    assert_eq!(f.engine_tip(), before);
    assert_eq!(f.calls(Endpoint::IndexedBlock), 1, "only the first sync's");

    f.sim.lock().settle_indexer();
    assert_eq!(f.sync().unwrap(), Sync::Synced { reverted: 0, applied: 1, reset: false });
}

#[test]
fn a_block_orphaned_during_the_walk_abandons_it() {
    let f = Fixture::new(grown_chain(instant_recovery()), 6);
    f.sync().unwrap();
    for seed in 1..=2 {
        f.sim.lock().mine(0, 1, seed);
    }
    let before = f.engine_tip();
    // The second block of the walk is orphaned just before it is fetched.
    let mut calls = 0;
    f.sim.on_call(move |endpoint, chain| {
        if endpoint == Endpoint::IndexedBlock {
            calls += 1;
            if calls == 2 {
                chain.reorg(2, 0, 7);
            }
        }
    });
    assert_eq!(f.sync().unwrap(), Sync::TipMoved);
    assert_eq!(f.engine_tip(), before, "nothing was sent");

    let synced = f.sync().unwrap();
    assert!(matches!(synced, Sync::Synced { reverted: 0, applied: 3, reset: false }), "{synced:?}");
    assert_eq!(f.engine_tip(), Some(f.sim.lock().tip()));
}

#[test]
fn a_recovering_indexer_sends_nothing_until_it_answers() {
    let f = Fixture::new(grown_chain(SimConfig::default()), 4);
    f.sync().unwrap();
    let key = f.key(2);
    f.load(&key);
    let before = f.engine_tip();
    // A same-height reorg orphans the indexer's tip: its next reads answer 503.
    f.sim.lock().orphan(1);
    f.sim.lock().mine(0, 2, 5);
    for _ in 0..SimConfig::default().recovery_reads {
        let error = f.sync().unwrap_err();
        assert!(matches!(error, NodeError::Unavailable { retry_after: Some(_) }), "{error:?}");
        assert_eq!(f.engine_tip(), before);
    }
    assert_eq!(f.sync().unwrap(), Sync::Synced { reverted: 1, applied: 1, reset: false });
    assert!(!f.engine.lock().unwrap().is_tracked(&key), "its load block was orphaned");
}

#[test]
fn a_reorg_as_deep_as_the_window_keeps_keys_loaded_below_it() {
    let f = Fixture::new(grown_chain(instant_recovery()), 2);
    f.sync().unwrap();
    let key = f.key(0);
    f.load(&key);
    for seed in 1..=2 {
        f.sim.lock().mine(1, 1, seed);
    }
    f.sync().unwrap();
    assert_eq!(f.chain_len(), 2);
    // Both blocks of the engine's chain are orphaned: the fork point is the parent of the
    // oldest, which a revert of every block reaches.
    assert_eq!(f.sim.lock().reorg(2, 0, 3), 2);
    assert_eq!(f.sync().unwrap(), Sync::Synced { reverted: 2, applied: 3, reset: false });
    assert!(f.engine.lock().unwrap().is_tracked(&key));
    f.assert_key_follows_the_oracle(&key);
}

#[test]
fn a_reorg_deeper_than_the_window_resyncs_every_key() {
    let f = Fixture::new(grown_chain(instant_recovery()), 2);
    f.sync().unwrap();
    let key = f.key(0);
    f.load(&key);
    for seed in 1..=2 {
        f.sim.lock().mine(1, 1, seed);
    }
    f.sync().unwrap();
    assert_eq!(f.sim.lock().reorg(3, 0, 3), 3);
    assert_eq!(f.sync().unwrap(), Sync::Synced { reverted: 3, applied: 1, reset: true });
    assert!(!f.engine.lock().unwrap().is_tracked(&key));
    assert_eq!(f.engine_tip(), Some(f.sim.lock().tip()));
    assert_eq!(f.chain_len(), 1, "a new start at the node's tip");
}
