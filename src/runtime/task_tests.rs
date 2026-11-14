//! The engine task against a reference: the same events, queries and subscriptions applied to
//! a plain `Engine`, with the routing rules written out plainly. Every reply must match the
//! reference's (an event's answer is what the reference routed to no one), and every sink
//! must hold exactly what the reference routed to it, in order.
//!
//! Replies cover the subscriber counts too: a count that drifted from the registry changes
//! which keys go idle, and with them the outputs of later blocks.

use std::{
    collections::{BTreeMap, VecDeque},
    sync::Mutex,
    time::Duration,
};

use proptest::{prelude::*, sample::Index};
use tokio::{sync::mpsc::error::TryRecvError, time::Instant};

use super::*;
use crate::{
    adapters::{
        AdapterError, BlockConsumer,
        follower::{self, Follower},
        loader::{self, LoadError, Loader},
    },
    clients::NodeError,
    engine::Config,
    sim::{Chain, SimChain, SimConfig, SimOp, strategy, testing::new_chain},
};

/// Small enough that reorgs reach past the window and keys go idle.
const CONFIG: Config = Config { window: 4, grace: 2 };
const MAX_REORG: u8 = 5;
const CASES: u32 = 1_000;

#[derive(Debug, Clone)]
enum Op {
    Chain(SimOp),
    /// One follower tick, a second after the last.
    Tick,
    /// One load attempt of a key.
    Load(Index),
    /// The indexer ends any recovery and catches up.
    Settle,
    /// A subscribe to a key, from a block the engine has been at (none: balance-only), with
    /// a sink of `capacity`.
    Subscribe {
        key: Index,
        from: Option<Index>,
        capacity: usize,
    },
    /// An unsubscribe of any subscription made so far, ended or not.
    Unsubscribe(Index),
    /// A subscriber reads everything its sink holds.
    Drain(Index),
    /// A subscriber goes away without unsubscribing.
    Leave(Index),
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        5 => strategy::op(MAX_REORG).prop_map(Op::Chain),
        5 => Just(Op::Tick),
        3 => any::<Index>().prop_map(Op::Load),
        1 => Just(Op::Settle),
        4 => (any::<Index>(), any::<Option<Index>>(), 1..=6usize)
            .prop_map(|(key, from, capacity)| Op::Subscribe { key, from, capacity }),
        1 => any::<Index>().prop_map(Op::Unsubscribe),
        3 => any::<Index>().prop_map(Op::Drain),
        1 => any::<Index>().prop_map(Op::Leave),
    ]
}

/// What random histories reached, for the coverage test.
#[derive(Debug, Default, Clone, Copy)]
struct Seen {
    live_from: bool,
    live_balance_only: bool,
    not_tracked: bool,
    catch_up_resync: bool,
    filtered: bool,
    full: bool,
    left: bool,
    swept: bool,
    resync_routed: bool,
    stale_unsubscribe: bool,
}

/// A subscription as the reference sees it.
#[derive(Debug)]
struct Expected {
    key: TrackingKey,
    balance_only: bool,
    capacity: usize,
    /// What its sink holds.
    queue: VecDeque<Output>,
    /// The receiver was dropped.
    left: bool,
    /// The task no longer has it: its sink was dropped.
    ended: bool,
}

/// The engine and the registry, as plainly as they can be written.
struct Reference {
    engine: Engine,
    next: u64,
    subscriptions: BTreeMap<u64, Expected>,
    seen: Seen,
}

impl Reference {
    fn subscribe(
        &mut self,
        key: TrackingKey,
        from: Option<BlockRef>,
        capacity: usize,
    ) -> Subscription {
        let outputs = self.engine.apply(Event::Subscribed(key.clone())).unwrap();
        if let [Output::ResyncRequired { reason, .. }] = outputs.as_slice() {
            self.seen.not_tracked = true;
            return Subscription::Resync(*reason);
        }
        assert!(outputs.is_empty());
        let Reply::CatchUp(catch_up) = self.engine.query(Query::CatchUp { key: key.clone(), from })
        else {
            unreachable!()
        };
        if let [Output::ResyncRequired { reason, .. }] = catch_up.as_slice() {
            self.seen.catch_up_resync = true;
            self.engine.apply(Event::Unsubscribed(key)).unwrap();
            return Subscription::Resync(*reason);
        }
        let id = self.next;
        self.next += 1;
        self.seen.live_from |= from.is_some();
        self.seen.live_balance_only |= from.is_none();
        self.subscriptions.insert(
            id,
            Expected {
                key,
                balance_only: from.is_none(),
                capacity,
                queue: VecDeque::new(),
                left: false,
                ended: false,
            },
        );
        Subscription::Live { id: SubscriptionId(id), catch_up }
    }

    fn unsubscribe(&mut self, id: u64) {
        let expected = self.subscriptions.get_mut(&id).expect("made by the test");
        if expected.ended {
            self.seen.stale_unsubscribe = true;
            return;
        }
        expected.ended = true;
        let key = expected.key.clone();
        self.engine.apply(Event::Unsubscribed(key)).unwrap();
    }

    /// Before a block, every subscription whose receiver was dropped ends and is
    /// unsubscribed.
    fn sweep(&mut self) {
        for expected in self.subscriptions.values_mut() {
            if expected.left && !expected.ended {
                self.seen.swept = true;
                expected.ended = true;
                let key = expected.key.clone();
                self.engine.apply(Event::Unsubscribed(key)).unwrap();
            }
        }
    }

    /// Each output goes to every live subscription of its key that wants it, in order. A
    /// subscription that can't take it ends and is unsubscribed; a `resync_required` ends
    /// them all, unsubscribing none (the engine dropped the key). `LoadRejected` goes to no
    /// one, and is what the event's caller gets back.
    fn route(&mut self, outputs: &[Output]) -> Vec<Output> {
        let mut unrouted = Vec::new();
        for output in outputs {
            if matches!(output, Output::LoadRejected { .. }) {
                unrouted.push(output.clone());
                continue;
            }
            let resync = matches!(output, Output::ResyncRequired { .. });
            let balance = matches!(output, Output::Balance { .. });
            let mut unsubscribe = 0;
            for expected in self.subscriptions.values_mut() {
                if expected.ended || expected.key != *output.key() {
                    continue;
                }
                if !expected.balance_only || balance || resync {
                    if expected.left || expected.queue.len() == expected.capacity {
                        self.seen.full |= !expected.left;
                        self.seen.left |= expected.left;
                        expected.ended = true;
                        unsubscribe += usize::from(!resync);
                        continue;
                    }
                    expected.queue.push_back(output.clone());
                } else {
                    self.seen.filtered = true;
                }
                if resync {
                    self.seen.resync_routed = true;
                    expected.ended = true;
                }
            }
            for _ in 0..unsubscribe {
                let key = output.key().clone();
                assert!(
                    self.engine
                        .apply(Event::Unsubscribed(key))
                        .unwrap()
                        .is_empty()
                );
            }
        }
        unrouted
    }
}

/// The task and the reference behind one `EngineHandle`: each event and query goes to both,
/// and their answers must be equal.
struct Mirror<'a> {
    task: &'a Handle,
    reference: &'a Mutex<Reference>,
}

impl EngineHandle for Mirror<'_> {
    async fn apply(&self, event: Event) -> Result<Result<Vec<Output>, EngineError>, EngineGone> {
        let got = self.task.apply(event.clone()).await?;
        let mut reference = self.reference.lock().unwrap();
        if matches!(event, Event::BlockApplied(_)) {
            reference.sweep();
        }
        let want = reference
            .engine
            .apply(event)
            .map(|outputs| reference.route(&outputs));
        assert_eq!(got, want, "the task's answer differs from the reference's");
        Ok(got)
    }

    async fn query(&self, query: Query) -> Result<Reply, EngineGone> {
        let got = self.task.query(query.clone()).await?;
        let want = self.reference.lock().unwrap().engine.query(query);
        assert_eq!(got, want, "the task's reply differs from the engine's");
        Ok(got)
    }
}

struct Harness {
    sim: SimChain,
    task: Handle,
    reference: Mutex<Reference>,
    receivers: BTreeMap<u64, mpsc::Receiver<Arc<Output>>>,
    /// Every subscription made, for unsubscribes.
    ids: Vec<u64>,
    /// Blocks the engine has been at, for `from`.
    tips: Vec<BlockRef>,
    follower: Follower<SimChain>,
    loader: Loader<SimChain>,
    now: Instant,
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
            task: spawn(Engine::new(CONFIG)).0,
            reference: Mutex::new(Reference {
                engine: Engine::new(CONFIG),
                next: 0,
                subscriptions: BTreeMap::new(),
                seen: Seen::default(),
            }),
            receivers: BTreeMap::new(),
            ids: Vec::new(),
            tips: Vec::new(),
            now: Instant::now(),
            sim,
        }
    }

    fn mirror(&self) -> Mirror<'_> {
        Mirror { task: &self.task, reference: &self.reference }
    }

    async fn run(&mut self, op: &Op) {
        match op {
            Op::Chain(op) => self.sim.lock().apply(op),
            Op::Tick => {
                let mirror = Mirror { task: &self.task, reference: &self.reference };
                match self.follower.tick(&mirror, self.now).await {
                    Ok(_) | Err(AdapterError::Node(NodeError::Unavailable { .. })) => {}
                    Err(error) => panic!("unexpected tick error: {error}"),
                }
                self.now += Duration::from_secs(1);
                if let Reply::Tip(Some(tip)) = self.mirror().query(Query::Tip).await.unwrap()
                    && self.tips.last() != Some(&tip)
                {
                    self.tips.push(tip);
                }
            }
            Op::Load(index) => {
                let keys = self.sim.lock().keys();
                let key = index.get(&keys).clone();
                match self.loader.try_load(&self.mirror(), &key).await {
                    Ok(_) | Err(LoadError::Node(NodeError::Unavailable { .. })) => {}
                    Err(error) => panic!("unexpected load error: {error}"),
                }
            }
            Op::Settle => self.sim.lock().settle_indexer(),
            Op::Subscribe { key, from, capacity } => {
                let keys = self.sim.lock().keys();
                let key = key.get(&keys).clone();
                let from = from
                    .filter(|_| !self.tips.is_empty())
                    .map(|i| *i.get(&self.tips));
                let (sink, receiver) = mpsc::channel(*capacity);
                let got = self.task.subscribe(key.clone(), from, sink).await.unwrap();
                let want = self
                    .reference
                    .lock()
                    .unwrap()
                    .subscribe(key, from, *capacity);
                assert_eq!(got, want);
                if let Subscription::Live { id, .. } = got {
                    self.receivers.insert(id.0, receiver);
                    self.ids.push(id.0);
                }
            }
            Op::Unsubscribe(index) => {
                if !self.ids.is_empty() {
                    let id = *index.get(&self.ids);
                    self.task.unsubscribe(SubscriptionId(id)).await.unwrap();
                    self.reference.lock().unwrap().unsubscribe(id);
                }
            }
            Op::Drain(index) => {
                if !self.receivers.is_empty() {
                    let id = *index.get(&self.receivers.keys().copied().collect::<Vec<_>>());
                    self.drain(id);
                }
            }
            Op::Leave(index) => {
                if !self.receivers.is_empty() {
                    let id = *index.get(&self.receivers.keys().copied().collect::<Vec<_>>());
                    self.receivers.remove(&id);
                    self.reference
                        .lock()
                        .unwrap()
                        .subscriptions
                        .get_mut(&id)
                        .unwrap()
                        .left = true;
                }
            }
        }
    }

    /// The sink holds exactly what the reference routed to it, then its end if it ended.
    fn drain(&mut self, id: u64) {
        let receiver = self.receivers.get_mut(&id).unwrap();
        let mut got = Vec::new();
        let end = loop {
            match receiver.try_recv() {
                Ok(output) => got.push(Arc::unwrap_or_clone(output)),
                Err(end) => break end,
            }
        };
        let mut reference = self.reference.lock().unwrap();
        let expected = reference.subscriptions.get_mut(&id).unwrap();
        let want: Vec<Output> = expected.queue.drain(..).collect();
        assert_eq!(got, want, "subscription {id} on {:?}", expected.key);
        let ended = end == TryRecvError::Disconnected;
        assert_eq!(ended, expected.ended, "subscription {id} ended");
        if ended {
            self.receivers.remove(&id);
        }
    }

    async fn finish(&mut self) -> Seen {
        self.sim.lock().settle_indexer();
        for _ in 0..3 {
            self.run(&Op::Tick).await;
        }
        for id in self.receivers.keys().copied().collect::<Vec<_>>() {
            self.drain(id);
        }
        self.reference.lock().unwrap().seen
    }
}

fn run(ops: &[Op]) -> Seen {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    runtime.block_on(async {
        let mut h = Harness::new();
        for op in ops {
            h.run(op).await;
        }
        h.finish().await
    })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(CASES))]

    #[test]
    fn the_task_is_the_engine_plus_routing(ops in prop::collection::vec(op(), 0..=60)) {
        run(&ops);
    }
}

#[test]
fn random_histories_reach_the_routing_cases() {
    let mut runner = proptest::test_runner::TestRunner::deterministic();
    let strategy = prop::collection::vec(op(), 40..=60);
    let mut all = Seen::default();
    for _ in 0..300 {
        let ops = proptest::strategy::ValueTree::current(
            &proptest::strategy::Strategy::new_tree(&strategy, &mut runner).unwrap(),
        );
        let seen = run(&ops);
        all.live_from |= seen.live_from;
        all.live_balance_only |= seen.live_balance_only;
        all.not_tracked |= seen.not_tracked;
        all.catch_up_resync |= seen.catch_up_resync;
        all.filtered |= seen.filtered;
        all.full |= seen.full;
        all.left |= seen.left;
        all.swept |= seen.swept;
        all.resync_routed |= seen.resync_routed;
        all.stale_unsubscribe |= seen.stale_unsubscribe;
    }
    for (name, reached) in [
        ("a subscription with `from`", all.live_from),
        ("a balance-only subscription", all.live_balance_only),
        ("a subscribe to an untracked key", all.not_tracked),
        ("a catch-up that resyncs", all.catch_up_resync),
        ("an output a balance-only subscription skips", all.filtered),
        ("a full sink", all.full),
        ("a subscriber that left, found by routing", all.left),
        ("a subscriber that left, found before a block", all.swept),
        ("a routed `resync_required`", all.resync_routed),
        ("an unsubscribe of an ended subscription", all.stale_unsubscribe),
    ] {
        assert!(reached, "no random history reached {name}");
    }
}

/// A chain mined a little, and an engine task synced to its tip.
async fn synced() -> (SimChain, Handle, JoinHandle<()>) {
    let mut chain = new_chain();
    chain.mine(0, 2, 1);
    let sim = SimChain::new(chain);
    let (handle, task) = spawn(Engine::new(CONFIG));
    let tip = sim.lock().tip();
    BlockConsumer::new(sim.clone())
        .sync(&handle, tip)
        .await
        .unwrap();
    (sim, handle, task)
}

async fn mine_and_sync(sim: &SimChain, engine: &Handle, blocks: usize) {
    let consumer = BlockConsumer::new(sim.clone());
    for seed in 0..blocks {
        let tip = sim.lock().mine(0, 1, seed as u64);
        consumer.sync(engine, tip).await.unwrap();
    }
}

async fn is_tracked(engine: &Handle, key: &TrackingKey) -> bool {
    matches!(engine.query(Query::Balance(key.clone())).await.unwrap(), Reply::Balance(Some(_)))
}

#[tokio::test]
async fn a_stopped_task_answers_engine_gone() {
    let (sim, engine, task) = synced().await;
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());

    let key = sim.lock().keys()[0].clone();
    assert_eq!(engine.query(Query::Tip).await, Err(EngineGone));
    let event = Event::Subscribed(key.clone());
    assert_eq!(engine.apply(event).await, Err(EngineGone));
    let (sink, _receiver) = mpsc::channel(1);
    assert_eq!(engine.subscribe(key.clone(), None, sink).await, Err(EngineGone));
    assert_eq!(engine.unsubscribe(SubscriptionId(0)).await, Err(EngineGone));

    // The adapters pass it up instead of retrying.
    let follower = Follower::new(sim.clone(), follower::Config::default());
    assert_eq!(follower.run(&engine).await, EngineGone);
    let loader = Loader::new(sim.clone(), loader::Config::default());
    assert!(matches!(loader.load(&engine, &key).await, Err(LoadError::EngineGone(_))));
}

#[tokio::test]
async fn a_subscriber_that_leaves_before_its_reply_is_unsubscribed() {
    let (sim, engine, _task) = synced().await;
    let keys = sim.lock().keys();
    let loader = Loader::new(sim.clone(), loader::Config::default());
    for key in &keys[..2] {
        loader.load(&engine, key).await.unwrap();
    }

    // The subscribe is sent, then its caller gives up before the reply.
    let (sink, _receiver) = mpsc::channel(8);
    let mut abandoned = Box::pin(engine.subscribe(keys[0].clone(), None, sink));
    let polled = std::future::poll_fn(|cx| std::task::Poll::Ready(abandoned.as_mut().poll(cx)));
    assert!(polled.await.is_pending(), "sent, not answered yet");
    drop(abandoned);
    let (sink, _kept) = mpsc::channel(8);
    let live = engine.subscribe(keys[1].clone(), None, sink).await.unwrap();
    assert!(matches!(live, Subscription::Live { .. }));

    mine_and_sync(&sim, &engine, CONFIG.grace as usize + 1).await;
    assert!(!is_tracked(&engine, &keys[0]).await, "the abandoned subscribe left no count");
    assert!(is_tracked(&engine, &keys[1]).await);
}

#[tokio::test]
async fn outputs_reach_subscribers_before_the_apply_returns() {
    let (sim, engine, _task) = synced().await;
    let key = sim.lock().keys()[0].clone();
    Loader::new(sim.clone(), loader::Config::default())
        .load(&engine, &key)
        .await
        .unwrap();
    let tip = sim.lock().tip();
    let (sink, mut receiver) = mpsc::channel(8);
    let Subscription::Live { catch_up, .. } = engine
        .subscribe(key.clone(), Some(tip), sink)
        .await
        .unwrap()
    else {
        panic!("subscribed at the tip");
    };
    assert!(matches!(catch_up.last(), Some(Output::Balance { .. })), "{catch_up:?}");

    let block = sim.lock().mine(0, 1, 7);
    let indexed = sim.lock().indexed_block(&block.header_id).unwrap();
    let outputs = engine
        .apply(Event::BlockApplied(indexed))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(outputs, Vec::new(), "routed outputs are not sent back");
    let first = receiver
        .try_recv()
        .expect("delivered before the apply returned");
    assert!(matches!(&*first, Output::Block { block: b, .. } if *b == block), "{first:?}");
    while let Ok(output) = receiver.try_recv() {
        assert_eq!(*output.key(), key);
    }
}

#[tokio::test]
async fn a_subscriber_that_leaves_a_quiet_key_lets_it_go_idle() {
    let (sim, engine, _task) = synced().await;
    let key = sim.lock().keys()[0].clone();
    Loader::new(sim.clone(), loader::Config::default())
        .load(&engine, &key)
        .await
        .unwrap();
    let (sink, receiver) = mpsc::channel(8);
    let live = engine.subscribe(key.clone(), None, sink).await.unwrap();
    assert!(matches!(live, Subscription::Live { .. }));
    drop(receiver);

    // Empty blocks: the key's balance never changes, so nothing is ever sent to the sink.
    let consumer = BlockConsumer::new(sim.clone());
    for _ in 0..=CONFIG.grace {
        let tip = sim.lock().mine_txs(Vec::new());
        consumer.sync(&engine, tip).await.unwrap();
    }
    assert!(!is_tracked(&engine, &key).await, "the dropped receiver left no count");
}

#[tokio::test]
async fn a_rejected_load_goes_only_to_its_caller() {
    let (sim, engine, _task) = synced().await;
    let key = sim.lock().keys()[0].clone();
    Loader::new(sim.clone(), loader::Config::default())
        .load(&engine, &key)
        .await
        .unwrap();
    let (sink, mut receiver) = mpsc::channel(8);
    let tip = sim.lock().tip();
    let live = engine
        .subscribe(key.clone(), Some(tip), sink)
        .await
        .unwrap();
    assert!(matches!(live, Subscription::Live { .. }));

    // A load pinned to a block that is not the tip, as when a block event wins the race.
    let stale = BlockRef {
        height: tip.height - 1,
        header_id: sim.lock().header_at(tip.height - 1).unwrap().header_id,
    };
    let event = Event::KeyLoaded { key: key.clone(), as_of: stale, boxes: Vec::new() };
    let outputs = engine.apply(event).await.unwrap().unwrap();
    assert_eq!(outputs, vec![Output::LoadRejected { key, as_of: stale }]);
    assert_eq!(receiver.try_recv().unwrap_err(), TryRecvError::Empty);
}
