//! The adapters together against `SimChain`: random histories with follower ticks and loads
//! in between, feeding one real `Engine`. The engine never refuses what they send, and once
//! the indexer settles it ends at the node's tip with every tracked key equal to the oracle.

use std::sync::Mutex;

use proptest::{prelude::*, sample::Index};

use super::*;
use crate::{
    adapters::{
        blocks::Sync,
        follower::{self, Follower},
        loader::{self, Attempt, LoadError, Loader},
        mempool::Poll,
    },
    engine::{Config, TrackingKey},
    sim::{Chain, SimChain, SimConfig, SimOp, strategy},
};

/// Small enough that reorgs reach past the window and keys go idle.
const CONFIG: Config = Config { window: 4, grace: 2 };
const MAX_REORG: u8 = 5;
const CASES: u32 = 2_000;

#[derive(Debug, Clone)]
enum Op {
    Chain(SimOp),
    /// One follower tick, a second after the last.
    Tick,
    /// One load attempt of a key.
    Load(Index),
    /// The indexer ends any recovery and catches up.
    Settle,
    /// The tip is replaced by another block at the same height. `SimOp::Reorg` always mines a
    /// longer branch, so the indexer's stale height stays below the tip and the consumer
    /// waits; at the same height it reads the indexer and gets its `503`s.
    Fork(u64),
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        6 => strategy::op(MAX_REORG).prop_map(Op::Chain),
        6 => Just(Op::Tick),
        3 => any::<Index>().prop_map(Op::Load),
        1 => Just(Op::Settle),
        1 => any::<u64>().prop_map(Op::Fork),
    ]
}

/// What random histories reached, for the coverage test.
#[derive(Debug, Default)]
struct Seen {
    reorg: bool,
    reset: bool,
    waiting: bool,
    unavailable: bool,
    snapshot: bool,
    tracked: bool,
    engine_behind: bool,
}

struct Harness {
    sim: SimChain,
    engine: Mutex<Engine>,
    follower: Follower<SimChain>,
    loader: Loader<SimChain>,
    now: tokio::time::Instant,
    seen: Seen,
}

impl Harness {
    fn new() -> Self {
        let sim = SimChain::new(Chain::new(SimConfig::default()));
        Self {
            follower: Follower::new(sim.clone(), follower::Config::default()),
            loader: Loader::new(
                sim.clone(),
                loader::Config { page_limit: 2, ..Default::default() },
            ),
            engine: Mutex::new(Engine::new(CONFIG)),
            now: tokio::time::Instant::now(),
            seen: Seen::default(),
            sim,
        }
    }

    fn run(&mut self, op: &Op) {
        match op {
            Op::Chain(op) => self.sim.lock().apply(op),
            Op::Tick => self.tick(),
            Op::Load(index) => self.load(index),
            Op::Settle => self.sim.lock().settle_indexer(),
            Op::Fork(seed) => {
                let mut chain = self.sim.lock();
                if chain.orphan(1) == 1 {
                    chain.mine(1, 2, *seed);
                }
            }
        }
    }

    fn tick(&mut self) {
        let tick =
            ready(self.follower.tick(&self.engine, self.now)).expect("SimChain serves /info");
        self.now += Duration::from_secs(1);
        let node_tip = self.sim.lock().tip();
        match &tick.blocks {
            Ok(Sync::Refused(error)) => panic!("the engine refused a block event: {error}"),
            Ok(Sync::Idle) => {}
            Ok(Sync::Synced { reverted, reset, .. }) => {
                self.seen.reorg |= *reverted > 0;
                self.seen.reset |= *reset;
            }
            Ok(Sync::WaitingForIndexer { .. }) => self.seen.waiting = true,
            Ok(Sync::TipMoved) => panic!("nothing moves the tip during a tick"),
            Err(NodeError::Unavailable { .. }) => self.seen.unavailable = true,
            Err(error) => panic!("unexpected node error: {error}"),
        }
        if matches!(tick.blocks, Ok(Sync::Idle | Sync::Synced { .. })) {
            assert_eq!(self.engine_tip(), Some(node_tip), "a sync reaches the node's tip");
        }
        match &tick.mempool {
            None | Some(Ok(_)) => {}
            Some(Err(error)) => panic!("unexpected mempool error: {error}"),
        }
        self.seen.snapshot |= matches!(tick.mempool, Some(Ok(Poll::Snapshot { .. })));
    }

    fn load(&mut self, index: &Index) {
        let keys = self.sim.lock().keys();
        let key = index.get(&keys).clone();
        match ready(self.loader.try_load(&self.engine, &key)) {
            Ok(Attempt::Tracked { as_of, .. }) => {
                self.seen.tracked = true;
                assert_eq!(Some(as_of), self.engine_tip());
                self.check_key(&key);
            }
            Ok(Attempt::EngineBehind { .. }) => self.seen.engine_behind = true,
            // The engine takes blocks only through the indexer, so the indexer is never below
            // an engine at the tip; and nothing moves between a load's reads here. Both are
            // hand-driven in `loader_tests.rs`.
            Ok(outcome) => panic!("unreachable between ops: {outcome:?}"),
            // A recovering indexer is reached only with the engine behind, which the load
            // checks first; `a_recovering_indexer_is_a_wait` drives it by hand.
            Err(LoadError::Node(NodeError::Unavailable { .. })) => {}
            Err(error) => panic!("unexpected load error: {error}"),
        }
    }

    fn engine_tip(&self) -> Option<BlockRef> {
        self.engine.lock().unwrap().tip()
    }

    fn query(&self, query: Query) -> Reply {
        self.engine.lock().unwrap().query(query)
    }

    /// The key's boxes and totals in the engine equal the oracle's at the engine's tip.
    fn check_key(&self, key: &TrackingKey) {
        let tip = self.engine_tip().expect("a tip");
        let oracle = self.sim.lock().oracle_at(&tip.header_id);
        let expected: Vec<_> = oracle.unspent(key, tip.height).into_keys().collect();
        let query = Query::Page { key: key.clone(), at: tip, after: None, limit: usize::MAX };
        match self.query(query) {
            Reply::Page(Ok(boxes)) => assert_eq!(boxes, expected, "{key:?}"),
            other => panic!("{other:?}"),
        }
        match self.query(Query::Balance(key.clone())) {
            Reply::Balance(Some(balance)) => {
                assert_eq!(balance.confirmed, oracle.totals(key, tip.height), "{key:?}");
            }
            other => panic!("{other:?}"),
        }
    }

    /// Settles the indexer, ticks until the engine is at the node's tip, and checks it.
    fn settle_and_check(&mut self) {
        self.sim.lock().settle_indexer();
        for _ in 0..3 {
            self.tick();
        }
        let tip = self.sim.lock().tip();
        assert_eq!(self.engine_tip(), Some(tip));
        let mempool = self.sim.lock().mempool();
        match self.query(Query::MempoolSnapshot) {
            Reply::MempoolSnapshot(Some((txs, at))) => {
                assert_eq!(at, tip);
                assert_eq!(&*txs, mempool.as_slice());
            }
            other => panic!("no snapshot at the tip: {other:?}"),
        }
        let oracle = self.sim.lock().oracle();
        let keys = self.sim.lock().keys();
        for key in keys {
            if self.engine.lock().unwrap().is_tracked(&key) {
                self.check_key(&key);
            }
            match self.query(Query::Overlay(key.clone())) {
                Reply::Overlay(overlay) => {
                    assert_eq!(overlay, oracle.overlay(&key, &mempool), "{key:?}");
                }
                other => panic!("{other:?}"),
            }
        }
    }
}

fn run(ops: &[Op]) -> Seen {
    let mut h = Harness::new();
    for op in ops {
        h.run(op);
    }
    h.settle_and_check();
    h.seen
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(CASES))]

    #[test]
    fn the_adapters_bring_the_engine_to_the_oracle(ops in prop::collection::vec(op(), 0..=60)) {
        run(&ops);
    }
}

#[test]
fn random_histories_reach_the_hard_adapter_cases() {
    let mut runner = proptest::test_runner::TestRunner::deterministic();
    let strategy = prop::collection::vec(op(), 40..=60);
    let mut all = Seen::default();
    for _ in 0..300 {
        let ops = proptest::strategy::ValueTree::current(
            &proptest::strategy::Strategy::new_tree(&strategy, &mut runner).unwrap(),
        );
        let seen = run(&ops);
        all.reorg |= seen.reorg;
        all.reset |= seen.reset;
        all.waiting |= seen.waiting;
        all.unavailable |= seen.unavailable;
        all.snapshot |= seen.snapshot;
        all.tracked |= seen.tracked;
        all.engine_behind |= seen.engine_behind;
    }
    let Seen { reorg, reset, waiting, unavailable, snapshot, tracked, engine_behind } = all;
    for (name, reached) in [
        ("a reorg", reorg),
        ("a fork beyond the window", reset),
        ("a wait for the indexer", waiting),
        ("a 503 during a sync", unavailable),
        ("a mempool snapshot", snapshot),
        ("a tracked load", tracked),
        ("a load with the engine behind", engine_behind),
    ] {
        assert!(reached, "no random history reached {name}");
    }
}

#[test]
fn backoff_waits_for_retry_after_on_a_503_only() {
    let fallback = Duration::from_secs(1);
    let unavailable = NodeError::Unavailable { retry_after: Some(Duration::from_secs(5)) };
    assert_eq!(backoff(&unavailable, fallback), Duration::from_secs(5));
    let no_header = NodeError::Unavailable { retry_after: None };
    assert_eq!(backoff(&no_header, fallback), fallback);
    let not_indexed = NodeError::NotIndexed(crate::clients::node::IndexedHeightResponse {
        indexed_height: 1,
        full_height: 2,
    });
    assert_eq!(backoff(&not_indexed, fallback), fallback);
}

use crate::sim::testing::ready;
