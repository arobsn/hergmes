//! Layer 1: the engine against the oracle over random histories, driven the way the adapters
//! and the transport drive it, with simulated clients that apply what they are sent.

use std::collections::{BTreeSet, HashSet};

use proptest::{prelude::*, sample::Index};

use super::*;
use crate::{
    engine::PendingTx,
    sim::{self, Chain, Oracle, SimConfig, SimOp, strategy, testing::reference_delta},
    types::ergo::{TxId, UnconfirmedTransaction},
};

/// Small enough that random histories reach the window and grace edges: reorgs go up to 5
/// blocks deep.
const CONFIG: Config = Config { window: 4, grace: 2 };
const MAX_REORG: u8 = 5;
const CASES: u32 = 5_000;

#[derive(Debug, Clone)]
enum Op {
    /// The node moves on; the engine does not see it yet.
    Chain(SimOp),
    /// The block consumer brings the engine to the node's tip.
    Sync,
    /// One step of it: a revert or an apply.
    SyncOne,
    /// The mempool poller sends the node's mempool, observed at the node's tip.
    Snapshot,
    /// The loader loads a key at the node's tip.
    Load(Index),
    /// A new client: a snapshot through pages (or none, for a balance-only one), a
    /// subscription, and its catch-up.
    Subscribe {
        key: Index,
        balance_only: bool,
    },
    Unsubscribe(Index),
    /// A client disconnects, keeping its state to resume.
    Disconnect(Index),
    /// A disconnected client subscribes again from the last block it applied.
    Resume(Index),
    /// A REST read of a key's pages at a block the engine has seen, main or orphaned.
    Page {
        key: Index,
        at: Index,
        limit: u8,
    },
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        6 => strategy::op(MAX_REORG).prop_map(Op::Chain),
        4 => Just(Op::Sync),
        2 => Just(Op::SyncOne),
        4 => Just(Op::Snapshot),
        2 => any::<Index>().prop_map(Op::Load),
        4 => (any::<Index>(), any::<bool>())
            .prop_map(|(key, balance_only)| Op::Subscribe { key, balance_only }),
        1 => any::<Index>().prop_map(Op::Unsubscribe),
        2 => any::<Index>().prop_map(Op::Disconnect),
        2 => any::<Index>().prop_map(Op::Resume),
        2 => (any::<Index>(), any::<Index>(), 1..5u8)
            .prop_map(|(key, at, limit)| Op::Page { key, at, limit }),
    ]
}

/// What the test expects of a tracked key, kept apart from the engine.
#[derive(Debug, Clone)]
struct KeyModel {
    /// Counts loads, so a client can tell whether the key stayed tracked since its snapshot.
    epoch: u64,
    since: BlockRef,
    activity: u32,
    subscribers: u32,
    /// The last balance sent (or delivered with a catch-up), and the last overlay.
    balance: (Totals, PendingDelta),
    overlay: Vec<PendingTx>,
}

#[derive(Debug, Clone)]
struct Client {
    key: TrackingKey,
    balance_only: bool,
    /// Subscribed and receiving messages.
    live: bool,
    /// Disconnected with its state, and may resume.
    resumable: bool,
    epoch: u64,
    tip: BlockRef,
    boxes: BTreeMap<BoxId, Arc<UTxO>>,
    overlay: Vec<PendingTx>,
    balance: Option<Balance>,
}

impl Client {
    /// Applies one message, checking what a client SDK checks.
    fn receive(&mut self, output: &Output, engine_tip: Option<BlockRef>) {
        match output {
            Output::Block { block, parent_id, created, spent, .. } if !self.balance_only => {
                assert_eq!(*parent_id, self.tip.header_id, "block {block:?} skips a block");
                assert_eq!(block.height, self.tip.height + 1);
                for s in spent {
                    assert!(self.boxes.remove(&s.box_id).is_some(), "spends unknown {}", s.box_id);
                }
                for b in created {
                    assert!(self.boxes.insert(b.id, b.clone()).is_none(), "creates held {}", b.id);
                }
                self.tip = *block;
            }
            Output::Rollback { block, parent_id, created, spent, .. } if !self.balance_only => {
                assert_eq!(*block, self.tip, "rolls back a block the client is not at");
                for s in spent {
                    assert!(self.boxes.remove(&s.box_id).is_some(), "undoes unknown {}", s.box_id);
                }
                for b in created {
                    assert!(self.boxes.insert(b.id, b.clone()).is_none(), "restores held {}", b.id);
                }
                self.tip = BlockRef { height: block.height - 1, header_id: *parent_id };
            }
            Output::Mempool { tip_height, pending, .. } if !self.balance_only => {
                assert_eq!(*tip_height, self.tip.height);
                // Every pending spend refers to a box the client has, or to one an earlier tx
                // of the overlay creates.
                let mut created = HashSet::new();
                for tx in pending {
                    for id in &tx.spent {
                        assert!(
                            self.boxes.contains_key(id) || created.contains(id),
                            "{} spends {id}, which the client does not have",
                            tx.tx_id
                        );
                    }
                    created.extend(tx.created.iter().map(|b| b.id));
                }
                self.overlay = pending.clone();
            }
            Output::Balance { balance, .. } => {
                let engine_tip = engine_tip.expect("a balance goes out with a tip");
                let tip = if self.balance_only { engine_tip } else { self.tip };
                assert_eq!(balance.as_of, tip, "a balance for another block");
                self.balance = Some(balance.clone());
            }
            Output::ResyncRequired { .. } => {
                self.live = false;
                self.resumable = false;
            }
            Output::Block { .. } | Output::Rollback { .. } | Output::Mempool { .. } => {}
            Output::LoadRejected { .. } => panic!("load results go to the loader"),
        }
    }
}

struct Harness {
    chain: Chain,
    engine: Engine,
    /// The blocks fed to the engine and not reverted, oldest first, ending at its tip.
    fed: Vec<BlockRef>,
    /// Blocks the engine reverted.
    reverted: Vec<BlockRef>,
    /// The model of the ring buffer's length.
    ring_len: u32,
    keys: BTreeMap<TrackingKey, KeyModel>,
    next_epoch: u64,
    /// The snapshot the engine uses, as received, and the one it holds for a later block.
    held: Option<Arc<[UnconfirmedTransaction]>>,
    ahead: Option<(Arc<[UnconfirmedTransaction]>, BlockRef)>,
    clients: Vec<Client>,
    seen: BTreeSet<&'static str>,
}

impl Harness {
    fn new() -> Self {
        let mut harness = Self {
            chain: Chain::new(SimConfig::default()),
            engine: Engine::new(CONFIG),
            fed: Vec::new(),
            reverted: Vec::new(),
            ring_len: 0,
            keys: BTreeMap::new(),
            next_epoch: 0,
            held: None,
            ahead: None,
            clients: Vec::new(),
            seen: BTreeSet::new(),
        };
        harness.sync(usize::MAX);
        harness
    }

    fn tip(&self) -> BlockRef {
        self.engine.tip().expect("the engine has a tip")
    }

    /// The oracle over the engine's chain.
    fn oracle(&self) -> Oracle {
        self.chain.oracle_at(&self.tip().header_id)
    }

    /// The lowest height the engine can serve, by the model of its ring.
    fn floor(&self) -> u32 {
        self.tip().height - self.ring_len
    }

    fn run(&mut self, op: &Op) {
        match op {
            Op::Chain(op) => self.chain.apply(op),
            Op::Sync => self.sync(usize::MAX),
            Op::SyncOne => self.sync(1),
            Op::Snapshot => self.snapshot(),
            Op::Load(key) => {
                let key = self.pick_key(key);
                self.load(&key);
            }
            Op::Subscribe { key, balance_only } => {
                let key = self.pick_key(key);
                self.subscribe(key, *balance_only);
            }
            Op::Unsubscribe(pick) => {
                if let Some(i) = self.pick_client(pick, |c| c.live) {
                    self.unsubscribe(i, false);
                }
            }
            Op::Disconnect(pick) => {
                if let Some(i) = self.pick_client(pick, |c| c.live) {
                    self.unsubscribe(i, true);
                }
            }
            Op::Resume(pick) => {
                if let Some(i) = self.pick_client(pick, |c| c.resumable) {
                    self.resume(i);
                }
            }
            Op::Page { key, at, limit } => {
                let key = self.pick_key(key);
                let blocks: Vec<BlockRef> =
                    self.fed.iter().chain(&self.reverted).copied().collect();
                let at = blocks[at.index(blocks.len())];
                self.page(&key, at, usize::from(*limit));
            }
        }
        self.check();
    }

    fn pick_key(&self, index: &Index) -> TrackingKey {
        let keys = self.chain.keys();
        keys[index.index(keys.len())].clone()
    }

    fn pick_client(&self, index: &Index, filter: impl Fn(&Client) -> bool) -> Option<usize> {
        let candidates: Vec<usize> = (0..self.clients.len())
            .filter(|&i| filter(&self.clients[i]))
            .collect();
        (!candidates.is_empty()).then(|| candidates[index.index(candidates.len())])
    }

    // --- Adapters ---

    /// Moves the engine up to `steps` blocks towards the node's tip, as the block consumer
    /// does: reverts its blocks off the node's chain, tip first, then applies the node's
    /// blocks, oldest first. With no tip (a fresh start, or after the ring ran out) it starts
    /// at the node's tip.
    fn sync(&mut self, steps: usize) {
        for _ in 0..steps {
            let Some(&tip) = self.fed.last() else {
                let start = self.chain.tip();
                self.apply_block(start);
                continue;
            };
            if self.chain.header_at(tip.height) != Some(tip) {
                self.revert_block(tip);
            } else if let Some(next) = self.chain.header_at(tip.height + 1) {
                self.apply_block(next);
            } else {
                return;
            }
        }
    }

    fn apply_block(&mut self, block: BlockRef) {
        let indexed = self
            .chain
            .oracle_at(&block.header_id)
            .indexed_block(block.height);
        let before = self.expected_states();
        let in_block = in_block_spends(&indexed);
        let outputs = self.feed(Event::BlockApplied(indexed));
        self.fed.push(block);
        self.ring_len = (self.ring_len + 1).min(CONFIG.window);

        // Every tracked key gets the block, in key order, and nothing else at rank 0.
        let blocks: Vec<&TrackingKey> = outputs
            .iter()
            .filter_map(|o| match o {
                Output::Block { key, block: b, .. } => {
                    assert_eq!(*b, block);
                    Some(key)
                }
                Output::Rollback { .. } | Output::ResyncRequired { .. } => {
                    panic!("an applied block sends {o:?}")
                }
                _ => None,
            })
            .collect();
        assert_eq!(blocks, before.keys().collect::<Vec<_>>());
        if outputs.iter().any(|o| {
            matches!(o, Output::Block { created, spent, .. }
            if created.iter().any(|b| in_block.contains(&b.id))
                || spent.iter().any(|s| in_block.contains(&s.box_id)))
        }) {
            panic!("a box created and spent within the block is in a delta");
        }
        if self
            .keys
            .keys()
            .any(|k| in_block_matches(&in_block, &self.chain, k))
        {
            self.seen
                .insert("tracked box created and spent within a block");
        }

        // The snapshot held for this block replaces the one in use.
        match self.ahead.take() {
            Some((txs, observed)) if observed == block => {
                self.held = Some(txs);
                self.seen.insert("snapshot held for a block used");
            }
            Some(ahead) if ahead.1.height > block.height => self.ahead = Some(ahead),
            _ => {}
        }

        // Idle keys go; keys with subscribers or recent activity stay.
        let idle: Vec<TrackingKey> = self
            .keys
            .iter()
            .filter(|(_, m)| {
                m.subscribers == 0 && block.height.saturating_sub(m.activity) > CONFIG.grace
            })
            .map(|(k, _)| k.clone())
            .collect();
        for key in idle {
            self.keys.remove(&key);
            self.seen.insert("idle key untracked");
        }
        if self
            .keys
            .values()
            .any(|m| block.height.saturating_sub(m.activity) > CONFIG.grace)
        {
            self.seen.insert("key kept by a subscriber");
        }
        self.check_emissions(&outputs, &before);
    }

    fn revert_block(&mut self, block: BlockRef) {
        let before = self.expected_states();
        let outputs = self.feed(Event::BlockReverted(block));
        self.fed.pop();
        self.reverted.push(block);
        self.held = None;

        let resyncs: BTreeSet<&TrackingKey> = outputs
            .iter()
            .filter_map(|o| match o {
                Output::ResyncRequired { key, reason } => {
                    assert_eq!(*reason, ResyncReason::ForkBeyondWindow);
                    Some(key)
                }
                _ => None,
            })
            .collect();
        if self.ring_len == 0 {
            // The ring ran out: every key goes, and the engine starts again from a tip.
            assert_eq!(resyncs, before.keys().collect());
            assert_eq!(self.engine.tip(), None);
            self.keys.clear();
            self.fed.clear();
            self.ahead = None;
            self.ring_len = 0;
            self.seen.insert("ring ran out");
            return;
        }
        self.ring_len -= 1;
        if self.fed.is_empty() {
            // The engine is at the reverted block's parent, which it never applied.
            self.fed.push(self.tip());
        }
        let dropped: BTreeSet<&TrackingKey> = self
            .keys
            .iter()
            .filter(|(_, m)| m.since == block)
            .map(|(k, _)| k)
            .collect();
        assert_eq!(resyncs, dropped, "exactly the keys loaded at the block are dropped");
        if !dropped.is_empty() {
            self.seen.insert("key dropped with its load block");
        }
        self.keys.retain(|_, m| m.since != block);

        let rollbacks: Vec<&TrackingKey> = outputs
            .iter()
            .filter_map(|o| match o {
                Output::Rollback { key, block: b, .. } => {
                    assert_eq!(*b, block);
                    Some(key)
                }
                _ => None,
            })
            .collect();
        assert_eq!(rollbacks, self.keys.keys().collect::<Vec<_>>());
        self.check_emissions(&outputs, &before);
    }

    fn snapshot(&mut self) {
        let txs: Arc<[UnconfirmedTransaction]> = self.chain.mempool().into();
        let observed = self.chain.tip();
        let before = self.expected_states();
        let outputs =
            self.feed(Event::MempoolSnapshot { txs: txs.clone(), observed_tip: observed });
        let tip = self.tip();
        if observed == tip {
            // Txs of orphaned blocks back in the pool reach overlays only from a snapshot
            // taken after the rollback.
            let returned = self.reverted.iter().any(|b| {
                let block = self.chain.block(&b.header_id).expect("mined");
                block.txs.iter().any(|t| txs.iter().any(|p| p.id == t.id))
            });
            if returned {
                self.seen
                    .insert("reorg-returned tx in a snapshot after the rollback");
            }
            self.held = Some(txs);
        } else if observed.height > tip.height {
            self.ahead = Some((txs, observed));
            assert!(outputs.is_empty());
        } else {
            assert!(outputs.is_empty());
        }
        self.check_emissions(&outputs, &before);
    }

    /// The loader: the key's boxes at the node's tip.
    fn load(&mut self, key: &TrackingKey) {
        let as_of = self.chain.tip();
        let boxes = self.chain.oracle().load(key, as_of.height);
        let before = self.expected_states();
        let tracked = self.keys.contains_key(key);
        let outputs = self.feed(Event::KeyLoaded { key: key.clone(), as_of, boxes });
        if self.engine.tip() != Some(as_of) {
            assert_eq!(outputs, vec![Output::LoadRejected { key: key.clone(), as_of }]);
            self.seen.insert("load rejected");
            return;
        }
        if tracked {
            self.keys.get_mut(key).expect("tracked").activity = as_of.height;
        } else {
            let (balance, overlay) = self.expected(key);
            self.next_epoch += 1;
            let model = KeyModel {
                epoch: self.next_epoch,
                since: as_of,
                activity: as_of.height,
                subscribers: 0,
                balance,
                overlay,
            };
            self.keys.insert(key.clone(), model);
        }
        self.check_emissions(&outputs, &before);
    }

    /// Feeds an event, checks the order of its messages and delivers them.
    fn feed(&mut self, event: Event) -> Vec<Output> {
        let outputs = self
            .engine
            .apply(event)
            .expect("the harness follows the engine's tip");
        let rank = |o: &Output| match o {
            Output::Mempool { .. } => 1,
            Output::Balance { .. } => 2,
            _ => 0,
        };
        let key = |o: &Output| match o {
            Output::Block { key, .. }
            | Output::Rollback { key, .. }
            | Output::Mempool { key, .. }
            | Output::Balance { key, .. }
            | Output::ResyncRequired { key, .. }
            | Output::LoadRejected { key, .. } => key.clone(),
        };
        for pair in outputs.windows(2) {
            let (a, b) = ((rank(&pair[0]), key(&pair[0])), (rank(&pair[1]), key(&pair[1])));
            assert!(a < b, "messages out of order: {:?} then {:?}", pair[0], pair[1]);
        }
        let tip = self.engine.tip();
        for output in outputs
            .iter()
            .filter(|o| !matches!(o, Output::LoadRejected { .. }))
        {
            for client in self
                .clients
                .iter_mut()
                .filter(|c| c.live && c.key == key(output))
            {
                client.receive(output, tip);
            }
        }
        outputs
    }

    // --- The transport ---

    fn subscribe(&mut self, key: TrackingKey, balance_only: bool) {
        if !self.engine.is_tracked(&key) {
            self.load(&key);
            if !self.engine.is_tracked(&key) {
                return;
            }
        }
        let tip = self.tip();
        let mut client = Client {
            key: key.clone(),
            balance_only,
            live: true,
            resumable: false,
            epoch: self.keys[&key].epoch,
            tip,
            boxes: BTreeMap::new(),
            overlay: Vec::new(),
            balance: None,
        };
        if !balance_only {
            let ids = self.pages(&key, tip, 3).expect("the tip is always served");
            client.boxes = ids
                .iter()
                .map(|k| (k.box_id, self.chain.find_box(&k.box_id).expect("a built box")))
                .collect();
        }
        let outputs = self.feed(Event::Subscribed(key.clone()));
        assert!(outputs.is_empty());
        let model = self.keys.get_mut(&key).expect("tracked");
        model.subscribers += 1;
        model.activity = tip.height;

        let from = (!balance_only).then_some(tip);
        let Reply::CatchUp(outputs) = self.engine.query(Query::CatchUp { key, from }) else {
            panic!("a catch-up reply");
        };
        // From the tip: nothing to replay, then the overlay (box subscriptions) and balance.
        assert_eq!(outputs.len(), if balance_only { 1 } else { 2 });
        for output in &outputs {
            client.receive(output, Some(tip));
        }
        self.clients.push(client);
    }

    fn unsubscribe(&mut self, i: usize, keep: bool) {
        let key = self.clients[i].key.clone();
        self.clients[i].live = false;
        self.clients[i].resumable = keep;
        assert!(self.feed(Event::Unsubscribed(key.clone())).is_empty());
        let height = self.tip().height;
        let model = self
            .keys
            .get_mut(&key)
            .expect("a key with a subscriber is tracked");
        model.subscribers -= 1;
        model.activity = height;
    }

    fn resume(&mut self, i: usize) {
        let key = self.clients[i].key.clone();
        self.clients[i].resumable = false;
        let outputs = self.feed(Event::Subscribed(key.clone()));
        if !self.keys.contains_key(&key) {
            assert_eq!(
                outputs,
                vec![Output::ResyncRequired { key, reason: ResyncReason::NotTracked }]
            );
            self.seen.insert("resync: not tracked");
            return;
        }
        assert!(outputs.is_empty());
        let tip = self.tip();
        let model = self.keys.get_mut(&key).expect("tracked");
        model.subscribers += 1;
        model.activity = tip.height;

        let client = &self.clients[i];
        let from = (!client.balance_only).then_some(client.tip);
        let expect_replay = client.balance_only
            || (client.epoch == self.keys[&key].epoch
                && self
                    .fork_point(client.tip)
                    .is_some_and(|f| f.height >= self.floor()));
        let Reply::CatchUp(outputs) = self.engine.query(Query::CatchUp { key: key.clone(), from })
        else {
            panic!("a catch-up reply");
        };
        if let [Output::ResyncRequired { reason, .. }] = outputs[..] {
            assert!(!expect_replay, "resync ({reason:?}) where a replay was possible");
            self.seen.insert(match reason {
                ResyncReason::OutsideWindow => "resync: outside window",
                ResyncReason::ForkBeyondWindow => "resync: fork beyond window",
                ResyncReason::NotTracked => "resync: not tracked",
                ResyncReason::SlowConsumer => unreachable!("the engine has no consumers"),
            });
            // The transport ends the subscription.
            assert!(self.feed(Event::Unsubscribed(key.clone())).is_empty());
            self.keys.get_mut(&key).expect("tracked").subscribers -= 1;
            return;
        }
        assert!(expect_replay, "a replay where the client's blocks are gone: {outputs:?}");
        if outputs.iter().any(|o| matches!(o, Output::Rollback { .. })) {
            self.seen.insert("catch-up rolls back an orphaned branch");
        }
        let client = &mut self.clients[i];
        client.live = true;
        for output in &outputs {
            client.receive(output, Some(tip));
        }
        if !client.balance_only {
            assert_eq!(client.tip, tip, "the catch-up reaches the tip");
        }
    }

    /// Where `block`'s branch meets the engine's chain, if it does above the first fed block.
    fn fork_point(&self, block: BlockRef) -> Option<BlockRef> {
        let first = self.fed.first()?;
        let mut cursor = block;
        loop {
            if cursor.height < first.height {
                return None;
            }
            if self.fed.get((cursor.height - first.height) as usize) == Some(&cursor) {
                return Some(cursor);
            }
            let parent = self.chain.block(&cursor.header_id)?.header.parent_id;
            cursor = BlockRef { height: cursor.height - 1, header_id: parent };
        }
    }

    /// Pages through `key` at `at`, `limit` ids at a time.
    fn pages(
        &mut self,
        key: &TrackingKey,
        at: BlockRef,
        limit: usize,
    ) -> Result<Vec<BoxKey>, PageError> {
        let mut ids = Vec::new();
        loop {
            let after = ids.last().copied();
            let query = Query::Page { key: key.clone(), at, after, limit };
            let Reply::Page(page) = self.engine.query(query) else {
                panic!("a page reply");
            };
            let page = page?;
            assert!(page.len() <= limit);
            let done = page.len() < limit;
            ids.extend(page);
            if done {
                return Ok(ids);
            }
        }
    }

    fn page(&mut self, key: &TrackingKey, at: BlockRef, limit: usize) {
        let result = self.pages(key, at, limit);
        let Some(model) = self.keys.get_mut(key) else {
            assert_eq!(result, Err(PageError::NotTracked));
            return;
        };
        model.activity = self.engine.tip().expect("a tracked key has a tip").height;
        let since = model.since;
        let on_main = self.fed.contains(&at);
        if !on_main || at.height < self.floor() || at.height < since.height {
            assert_eq!(result, Err(PageError::CursorExpired));
            self.seen.insert("page cursor expired");
            return;
        }
        let expected: Vec<BoxKey> = self.oracle().unspent(key, at.height).into_keys().collect();
        let ids = result.expect("a page on the main chain in the window");
        assert_eq!(ids, expected);
        if at != self.tip()
            && expected
                .iter()
                .any(|k| !self.engine.keys[&self.engine.ids[key]].boxes.contains(k))
        {
            self.seen
                .insert("page at an earlier block restores spent boxes");
        }
    }

    // --- Expectations ---

    /// The overlay the engine should hold for `key`: the oracle's over the snapshot in use,
    /// against the confirmed state at the engine's tip.
    fn reference_overlay(&self, key: &TrackingKey, utxo: &HashSet<BoxId>) -> Vec<PendingTx> {
        match &self.held {
            Some(txs) => sim::overlay(utxo, txs, key),
            None => Vec::new(),
        }
    }

    /// `key`'s balance values and overlay at the engine's tip.
    fn expected(&self, key: &TrackingKey) -> ((Totals, PendingDelta), Vec<PendingTx>) {
        let oracle = self.oracle();
        let utxo = oracle.utxo(self.tip().height);
        let ids: HashSet<BoxId> = utxo.values().map(|b| b.id).collect();
        expected_for(self, key, &utxo, &ids)
    }

    fn expected_states(&self) -> BTreeMap<TrackingKey, ((Totals, PendingDelta), Vec<PendingTx>)> {
        let Some(tip) = self.engine.tip() else {
            return BTreeMap::new();
        };
        let utxo = self.chain.oracle_at(&tip.header_id).utxo(tip.height);
        let ids: HashSet<BoxId> = utxo.values().map(|b| b.id).collect();
        self.keys
            .keys()
            .map(|k| (k.clone(), expected_for(self, k, &utxo, &ids)))
            .collect()
    }

    /// Each tracked key gets a `Mempool` message if and only if its overlay changed in the
    /// step, and a `Balance` if and only if its value did; `before` is from before the step.
    fn check_emissions(
        &mut self,
        outputs: &[Output],
        before: &BTreeMap<TrackingKey, ((Totals, PendingDelta), Vec<PendingTx>)>,
    ) {
        let after = self.expected_states();
        for (key, model) in &mut self.keys {
            let (balance, overlay) = &after[key];
            let mempool = outputs.iter().find_map(|o| match o {
                Output::Mempool { key: k, pending, .. } if k == key => Some(pending),
                _ => None,
            });
            match mempool {
                Some(pending) => {
                    assert_eq!(pending, overlay);
                    assert_ne!(&model.overlay, overlay, "an unchanged overlay sent");
                }
                None => assert_eq!(&model.overlay, overlay, "a changed overlay not sent"),
            }
            model.overlay = overlay.clone();

            let sent = outputs.iter().find_map(|o| match o {
                Output::Balance { key: k, balance } if k == key => Some(balance),
                _ => None,
            });
            match sent {
                Some(b) => {
                    assert_eq!((&b.confirmed, &b.pending), (&balance.0, &balance.1));
                    assert_ne!(&model.balance, balance, "an unchanged balance sent");
                }
                None => assert_eq!(&model.balance, balance, "a changed balance not sent"),
            }
            if let Some(((was, was_pending), _)) = before.get(key)
                && was != &balance.0
                && sum(was, was_pending) == sum(&balance.0, &balance.1)
            {
                self.seen
                    .insert("balance sent though confirmed + pending stayed");
            }
            model.balance = balance.clone();
        }
    }

    // --- Invariants after every op ---

    fn check(&mut self) {
        let tracked: Vec<&TrackingKey> = self.engine.ids.keys().collect();
        assert_eq!(tracked, self.keys.keys().collect::<Vec<_>>(), "tracked keys");
        let Some(tip) = self.engine.tip() else {
            assert!(self.keys.is_empty());
            return;
        };
        let oracle = self.chain.oracle_at(&tip.header_id);
        let utxo = oracle.utxo(tip.height);
        let ids: HashSet<BoxId> = utxo.values().map(|b| b.id).collect();

        // Each tracked key's set and running totals equal the oracle's, and the reverse index
        // holds exactly their boxes.
        let mut reverse = BTreeMap::new();
        for (key, &key_id) in &self.engine.ids {
            let state = &self.engine.keys[&key_id];
            let expected: BTreeSet<BoxKey> = oracle.unspent(key, tip.height).into_keys().collect();
            assert_eq!(state.boxes, expected, "{key:?}'s set");
            assert_eq!(state.totals, oracle.totals(key, tip.height), "{key:?}'s totals");
            for b in &state.boxes {
                reverse.insert((b.box_id, key_id), b.inclusion_height);
            }
            if state.boxes.iter().any(|b| {
                let held = &utxo[b];
                overlay::keys_of(held)
                    .filter(|k| self.engine.ids.contains_key(k))
                    .count()
                    >= 2
            }) {
                self.seen.insert("box held by two tracked keys");
            }
        }
        assert_eq!(self.engine.reverse, reverse, "reverse index");

        // Every key's overlay, tracked or not, is the oracle's over the snapshot in use.
        for key in self.chain.keys() {
            let reference = self.reference_overlay(&key, &ids);
            assert_eq!(self.engine.mempool.overlay(&key), reference, "{key:?}'s overlay");
        }

        // Every live client holds the oracle's state at the tip.
        for client in self.clients.iter().filter(|c| c.live) {
            let ((confirmed, pending), overlay) = expected_for(self, &client.key, &utxo, &ids);
            if !client.balance_only {
                assert_eq!(client.tip, tip);
                let expected: BTreeMap<BoxId, Arc<UTxO>> = oracle
                    .unspent(&client.key, tip.height)
                    .into_values()
                    .map(|b| (b.id, b))
                    .collect();
                assert_eq!(client.boxes, expected, "a client of {:?} diverged", client.key);
                assert_eq!(client.overlay, overlay);
            }
            let balance = client
                .balance
                .as_ref()
                .expect("a balance with the catch-up");
            assert_eq!((&balance.confirmed, &balance.pending), (&confirmed, &pending));
        }
        self.check_partial_confirmation(&ids);
    }

    fn check_partial_confirmation(&mut self, utxo: &HashSet<BoxId>) {
        let Some(txs) = &self.held else {
            return;
        };
        // A pending tx in the overlay spending an output of a snapshot tx a block mined.
        let order = sim::pending_order(utxo, txs);
        let listed: HashSet<TxId> = order.iter().map(|tx| tx.id).collect();
        let mined_outputs: HashSet<BoxId> = txs
            .iter()
            .filter(|tx| {
                !listed.contains(&tx.id) && tx.outputs.iter().any(|o| utxo.contains(&o.id))
            })
            .flat_map(|tx| tx.outputs.iter().map(|o| o.id))
            .collect();
        if order
            .iter()
            .any(|tx| tx.inputs.iter().any(|i| mined_outputs.contains(&i.id)))
        {
            self.seen.insert("partial confirmation");
        }
    }
}

fn sum(
    totals: &Totals,
    pending: &PendingDelta,
) -> (i128, BTreeMap<crate::types::ergo::TokenId, i128>) {
    let mut tokens: BTreeMap<_, i128> = totals
        .tokens
        .iter()
        .map(|(id, &a)| (*id, i128::from(a)))
        .collect();
    for (id, a) in &pending.tokens {
        *tokens.entry(*id).or_default() += a;
    }
    tokens.retain(|_, a| *a != 0);
    (i128::from(totals.nano_ergs) + pending.nano_ergs, tokens)
}

fn expected_for(
    harness: &Harness,
    key: &TrackingKey,
    utxo: &BTreeMap<BoxKey, Arc<UTxO>>,
    ids: &HashSet<BoxId>,
) -> ((Totals, PendingDelta), Vec<PendingTx>) {
    let held: Vec<&UTxO> = utxo
        .values()
        .map(|b| &**b)
        .filter(|b| key.matches(b))
        .collect();
    let overlay = harness.reference_overlay(key, ids);
    let totals = sim::totals(held.iter().copied());
    let delta = reference_delta(&held, &overlay);
    ((totals, delta), overlay)
}

/// Boxes a block both creates and spends.
fn in_block_spends(block: &IndexedBlock) -> HashSet<BoxId> {
    let created: HashSet<BoxId> = block
        .transactions
        .iter()
        .flat_map(|tx| tx.outputs.iter().map(|o| o.utxo.id))
        .collect();
    block
        .transactions
        .iter()
        .flat_map(|tx| tx.inputs.iter().map(|i| i.utxo.id))
        .filter(|id| created.contains(id))
        .collect()
}

fn in_block_matches(boxes: &HashSet<BoxId>, chain: &Chain, key: &TrackingKey) -> bool {
    boxes
        .iter()
        .any(|id| chain.find_box(id).is_some_and(|b| key.matches(&b)))
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(CASES))]

    /// After every op of a random history, the engine's state and every client's equal the
    /// oracle's, and the engine sends exactly the messages the step called for.
    #[test]
    fn the_engine_and_its_clients_follow_the_oracle(ops in prop::collection::vec(op(), 0..=60)) {
        let mut harness = Harness::new();
        for op in &ops {
            harness.run(op);
        }
    }
}

/// The property is only useful if random histories reach the cases that break engines.
#[test]
fn random_histories_reach_the_hard_engine_cases() {
    use proptest::{strategy::ValueTree, test_runner::TestRunner};
    let mut runner = TestRunner::deterministic();
    let mut seen = BTreeSet::new();
    for _ in 0..400 {
        let ops = prop::collection::vec(op(), 100)
            .new_tree(&mut runner)
            .expect("strategy")
            .current();
        let mut harness = Harness::new();
        for op in &ops {
            harness.run(op);
        }
        seen.extend(harness.seen);
    }
    for feature in [
        "tracked box created and spent within a block",
        "snapshot held for a block used",
        "idle key untracked",
        "ring ran out",
        "key dropped with its load block",
        "load rejected",
        "resync: not tracked",
        "resync: outside window",
        "resync: fork beyond window",
        "catch-up rolls back an orphaned branch",
        "page cursor expired",
        "page at an earlier block restores spent boxes",
        "box held by two tracked keys",
        "key kept by a subscriber",
        "partial confirmation",
        "balance sent though confirmed + pending stayed",
        "reorg-returned tx in a snapshot after the rollback",
    ] {
        assert!(seen.contains(feature), "no random history reached: {feature}");
    }
}

// --- Hand-driven cases ---

/// An engine following a `Chain` by hand.
struct Fixture {
    chain: Chain,
    engine: Engine,
}

impl Fixture {
    fn new(config: Config) -> Self {
        let chain = sim::testing::new_chain();
        let mut engine = Engine::new(config);
        let genesis = chain.oracle().indexed_block(chain.height());
        engine
            .apply(Event::BlockApplied(genesis))
            .expect("the first block");
        Self { chain, engine }
    }

    fn key(&self, tree: usize) -> TrackingKey {
        TrackingKey::Tree(self.chain.trees()[tree].clone())
    }

    fn mine(&mut self, txs: Vec<Arc<sim::SimTx>>) -> Vec<Output> {
        let block = self.chain.mine_txs(txs);
        let indexed = self.chain.oracle().indexed_block(block.height);
        self.engine
            .apply(Event::BlockApplied(indexed))
            .expect("extends the tip")
    }

    /// Orphans the tip on both sides.
    fn revert(&mut self) -> Vec<Output> {
        self.chain.orphan(1);
        let tip = self.engine.tip().expect("a tip");
        self.engine
            .apply(Event::BlockReverted(tip))
            .expect("the tip")
    }

    fn load(&mut self, key: &TrackingKey) -> Vec<Output> {
        let as_of = self.chain.tip();
        let boxes = self.chain.oracle().load(key, as_of.height);
        self.engine
            .apply(Event::KeyLoaded { key: key.clone(), as_of, boxes })
            .expect("no error")
    }

    fn snapshot(&mut self) -> Vec<Output> {
        let txs = self.chain.mempool().into();
        let observed_tip = self.chain.tip();
        self.engine
            .apply(Event::MempoolSnapshot { txs, observed_tip })
            .expect("no error")
    }

    fn catch_up(&mut self, key: &TrackingKey, from: Option<BlockRef>) -> Vec<Output> {
        let Reply::CatchUp(outputs) = self.engine.query(Query::CatchUp { key: key.clone(), from })
        else {
            panic!("a catch-up reply");
        };
        outputs
    }

    fn state(&self, key: &TrackingKey) -> (BTreeSet<BoxKey>, Totals) {
        let state = &self.engine.keys[&self.engine.ids[key]];
        (state.boxes.clone(), state.totals.clone())
    }
}

/// The message of kind `kind` for `key`.
fn find<'a>(outputs: &'a [Output], kind: &str, key: &TrackingKey) -> &'a Output {
    let at = kinds(outputs)
        .iter()
        .position(|(k, o)| *k == kind && o == key)
        .unwrap_or_else(|| panic!("no {kind} for {key:?} in {:?}", kinds(outputs)));
    &outputs[at]
}

fn kinds(outputs: &[Output]) -> Vec<(&'static str, TrackingKey)> {
    outputs
        .iter()
        .map(|o| match o {
            Output::Block { key, .. } => ("block", key.clone()),
            Output::Rollback { key, .. } => ("rollback", key.clone()),
            Output::Mempool { key, .. } => ("mempool", key.clone()),
            Output::Balance { key, .. } => ("balance", key.clone()),
            Output::ResyncRequired { key, .. } => ("resync", key.clone()),
            Output::LoadRejected { key, .. } => ("load_rejected", key.clone()),
        })
        .collect()
}

#[test]
fn a_block_moves_a_box_between_keys_and_its_rollback_moves_it_back() {
    let mut f = Fixture::new(Config::default());
    let (zero, one) = (f.key(0), f.key(1));
    f.load(&zero);
    f.load(&one);
    let before = (f.state(&zero), f.state(&one), f.engine.reverse.clone());

    let source = sim::testing::box_of(&f.chain, 0);
    let tx = f.chain.transfer(&[source], &f.chain.trees()[1].clone());
    let output = tx.outputs[0].clone();
    let outputs = f.mine(vec![tx.clone()]);
    let block = f.engine.tip().expect("a tip");
    let parent_id = f
        .chain
        .block(&block.header_id)
        .expect("mined")
        .header
        .parent_id;
    assert_eq!(outputs.len(), 4, "two blocks then two balances");
    assert_eq!(
        find(&outputs, "block", &zero),
        &Output::Block {
            key: zero.clone(),
            block,
            parent_id,
            created: vec![],
            spent: vec![Spend { box_id: source, tx_id: tx.id }],
        }
    );
    assert_eq!(
        find(&outputs, "block", &one),
        &Output::Block {
            key: one.clone(),
            block,
            parent_id,
            created: vec![output.clone()],
            spent: vec![]
        }
    );

    // The rollback is the delta with its sides swapped, and restores the state exactly.
    let outputs = f.revert();
    let spent_box = f.chain.find_box(&source).expect("built");
    assert_eq!(
        find(&outputs, "rollback", &zero),
        &Output::Rollback {
            key: zero.clone(),
            block,
            parent_id,
            created: vec![spent_box],
            spent: vec![],
        }
    );
    assert_eq!(
        find(&outputs, "rollback", &one),
        &Output::Rollback {
            key: one.clone(),
            block,
            parent_id,
            created: vec![],
            spent: vec![Spend { box_id: output.id, tx_id: tx.id }],
        }
    );
    assert_eq!((f.state(&zero), f.state(&one), f.engine.reverse.clone()), before);
}

#[test]
fn a_box_created_and_spent_in_one_block_is_in_no_delta() {
    let mut f = Fixture::new(Config::default());
    let (zero, one) = (f.key(0), f.key(1));
    f.load(&zero);
    f.load(&one);
    let source = sim::testing::box_of(&f.chain, 0);
    let first = f.chain.transfer(&[source], &f.chain.trees()[0].clone());
    let second = f
        .chain
        .transfer(&[first.outputs[0].id], &f.chain.trees()[1].clone());
    let outputs = f.mine(vec![first.clone(), second.clone()]);
    let Output::Block { created, spent, .. } = find(&outputs, "block", &zero) else {
        panic!("a block")
    };
    assert_eq!((created, spent), (&vec![], &vec![Spend { box_id: source, tx_id: first.id }]));
    let Output::Block { created, spent, .. } = find(&outputs, "block", &one) else {
        panic!("a block")
    };
    assert_eq!((created, spent), (&vec![second.outputs[0].clone()], &vec![]));
    let delta = f.engine.ring.back().expect("the tip's entry");
    assert!(!delta.boxes.contains_key(&first.outputs[0].id), "not stored");
}

/// The parent is mined, the child stays pending: after the block, the child spends the box
/// the block delivered, and the balance moves the parent's output from pending to confirmed.
#[test]
fn a_mined_parent_leaves_its_pending_child_in_the_overlay() {
    let mut f = Fixture::new(Config::default());
    let one = f.key(1);
    f.load(&one);
    let source = sim::testing::box_of(&f.chain, 0);
    let to_one = f.chain.trees()[1].clone();
    let parent = f.chain.transfer(&[source], &to_one);
    let child = f.chain.transfer(&[parent.outputs[0].id], &to_one);
    f.chain.submit(parent.clone());
    f.chain.submit(child.clone());
    let outputs = f.snapshot();
    assert_eq!(kinds(&outputs), [("mempool", one.clone()), ("balance", one.clone())]);
    let Output::Balance { balance: before, .. } = &outputs[1] else { panic!("a balance") };

    let outputs = f.mine(vec![parent.clone()]);
    assert_eq!(
        kinds(&outputs),
        [("block", one.clone()), ("mempool", one.clone()), ("balance", one.clone())]
    );
    let Output::Mempool { pending, .. } = &outputs[1] else { panic!("a mempool message") };
    let x = parent.outputs[0].id;
    assert_eq!(pending.len(), 1);
    assert_eq!((pending[0].tx_id, &pending[0].spent), (child.id, &vec![x]));
    let Output::Balance { balance: after, .. } = &outputs[2] else { panic!("a balance") };
    assert_eq!(after.confirmed.nano_ergs, before.confirmed.nano_ergs + parent.outputs[0].value);
    // The child keeps the value with the key, so the spendable total stays.
    assert_eq!(
        i128::from(after.confirmed.nano_ergs) + after.pending.nano_ergs,
        i128::from(before.confirmed.nano_ergs) + before.pending.nano_ergs
    );
}

/// The node's state can run ahead of its pool, so a snapshot can list a tx spending a box a
/// newly tracked key does not hold. Loading the key leaves that tx out, and every key whose
/// overlay changes is told.
#[test]
fn loading_a_key_decides_the_snapshot_again() {
    let mut f = Fixture::new(Config::default());
    let (zero, one) = (f.key(0), f.key(1));
    f.load(&one);
    let source = sim::testing::box_of(&f.chain, 0);
    let tx = f.chain.transfer(&[source], &f.chain.trees()[1].clone());
    f.chain.submit(tx);
    f.snapshot();
    assert_eq!(f.engine.mempool.overlay(&one).len(), 1);

    let as_of = f.chain.tip();
    let boxes = f
        .chain
        .oracle()
        .load(&zero, as_of.height)
        .into_iter()
        .filter(|b| b.utxo.id != source)
        .collect();
    let outputs = f
        .engine
        .apply(Event::KeyLoaded { key: zero, as_of, boxes })
        .expect("no error");
    assert_eq!(kinds(&outputs), [("mempool", one.clone()), ("balance", one.clone())]);
    assert!(f.engine.mempool.overlay(&one).is_empty());
}

#[test]
fn events_off_the_tip_are_refused_and_change_nothing() {
    let mut f = Fixture::new(Config::default());
    let zero = f.key(0);
    f.load(&zero);
    let genesis = f.engine.tip().expect("a tip");
    f.chain.mine(1, 1, 7);
    let next = f.chain.oracle().indexed_block(genesis.height + 1);
    f.chain.mine(1, 1, 8);
    let skipping = f.chain.oracle().indexed_block(genesis.height + 2);
    let state = f.state(&zero);

    let error = f
        .engine
        .apply(Event::BlockApplied(skipping))
        .expect_err("skips a block");
    assert!(matches!(error, EngineError::NotChild { .. }));
    let not_tip = BlockRef::from(&next.header);
    let error = f
        .engine
        .apply(Event::BlockReverted(not_tip))
        .expect_err("not the tip");
    assert!(matches!(error, EngineError::NotTip { .. }));
    assert_eq!((f.engine.tip(), f.state(&zero)), (Some(genesis), state));

    // A load pinned elsewhere is rejected.
    let one = f.key(1);
    let outputs = f.load(&one);
    let as_of = f.chain.tip();
    assert_eq!(outputs, [Output::LoadRejected { key: one.clone(), as_of }]);
    assert!(!f.engine.is_tracked(&one));
}

#[test]
fn an_untracked_key_is_refused_and_a_balance_only_catch_up_is_the_balance() {
    let mut f = Fixture::new(Config::default());
    let zero = f.key(0);
    let resync =
        vec![Output::ResyncRequired { key: zero.clone(), reason: ResyncReason::NotTracked }];
    assert_eq!(
        f.engine
            .apply(Event::Subscribed(zero.clone()))
            .expect("no error"),
        resync
    );
    let tip = f.engine.tip();
    assert_eq!(f.catch_up(&zero, tip), resync);
    assert_eq!(f.catch_up(&zero, None), resync);
    let Reply::Balance(None) = f.engine.query(Query::Balance(zero.clone())) else {
        panic!("no balance for an untracked key");
    };

    f.load(&zero);
    let outputs = f.catch_up(&zero, None);
    assert_eq!(kinds(&outputs), [("balance", zero.clone())]);
    let Reply::Balance(Some(balance)) = f.engine.query(Query::Balance(zero.clone())) else {
        panic!("a balance");
    };
    assert_eq!(outputs, [Output::Balance { key: zero, balance }]);
}

/// A client left on a branch that was orphaned after the key stopped being tracked: the
/// orphaned block's delta for the key was never kept, so the engine cannot roll it back,
/// even though the key is tracked again from the fork point.
#[test]
fn a_client_from_an_earlier_tracking_period_resyncs() {
    let mut f = Fixture::new(Config { window: 10, grace: 0 });
    let zero = f.key(0);
    f.load(&zero);
    f.engine
        .apply(Event::Subscribed(zero.clone()))
        .expect("no error");
    let source = sim::testing::box_of(&f.chain, 0);
    let tx = f.chain.transfer(&[source], &f.chain.trees()[1].clone());
    f.mine(vec![tx]);
    let client_at = f.engine.tip().expect("a tip");
    f.engine
        .apply(Event::Unsubscribed(zero.clone()))
        .expect("no error");
    f.mine(vec![]);
    assert!(!f.engine.is_tracked(&zero), "idle past the grace period");

    f.revert();
    f.revert();
    f.load(&zero);
    let outputs = f.catch_up(&zero, Some(client_at));
    assert_eq!(outputs, [Output::ResyncRequired { key: zero, reason: ResyncReason::NotTracked }]);
}

/// Pages pinned to an earlier block keep their ids while blocks land between page reads.
#[test]
fn pages_pinned_to_a_block_survive_later_blocks() {
    let mut f = Fixture::new(Config::default());
    let zero = f.key(0);
    f.load(&zero);
    let at = f.engine.tip().expect("a tip");
    let expected: Vec<BoxKey> = f
        .chain
        .oracle()
        .unspent(&zero, at.height)
        .into_keys()
        .collect();
    assert!(expected.len() >= 2);

    let page = |f: &mut Fixture, after| {
        let query = Query::Page { key: zero.clone(), at, after, limit: 1 };
        let Reply::Page(Ok(ids)) = f.engine.query(query) else { panic!("a page") };
        ids
    };
    let first = page(&mut f, None);
    // Spend the box the next page would return, and add one to the key.
    let next = expected[1].box_id;
    let tx = f.chain.transfer(&[next], &f.chain.trees()[0].clone());
    f.mine(vec![tx]);
    let second = page(&mut f, first.last().copied());
    assert_eq!([first, second].concat(), expected[..2]);
}

/// A key dropped with its load block and loaded again at the parent: the orphaned block was
/// the last applied before the new load, so its `seq` equals the key's `loaded_seq`, and it
/// still predates the load.
#[test]
fn a_client_at_an_orphaned_load_block_resyncs() {
    let mut f = Fixture::new(Config::default());
    let zero = f.key(0);
    let source = sim::testing::box_of(&f.chain, 0);
    let tx = f.chain.transfer(&[source], &f.chain.trees()[1].clone());
    f.mine(vec![tx]);
    f.load(&zero);
    let client_at = f.engine.tip().expect("a tip");

    let outputs = f.revert();
    let dropped =
        Output::ResyncRequired { key: zero.clone(), reason: ResyncReason::ForkBeyondWindow };
    assert_eq!(outputs, [dropped]);
    f.load(&zero);
    let outputs = f.catch_up(&zero, Some(client_at));
    assert_eq!(outputs, [Output::ResyncRequired { key: zero, reason: ResyncReason::NotTracked }]);
}

/// A snapshot can list a tx spending a box a block already spent, if the node's pool lags its
/// state. For a box a tracked key held, the engine checks its own set, so the tx is in no
/// overlay, whether the snapshot arrives at the tip or was held for the block.
#[test]
fn a_pending_spend_of_a_box_a_block_spent_is_left_out() {
    for held in [false, true] {
        let mut f = Fixture::new(Config::default());
        let (zero, one) = (f.key(0), f.key(1));
        f.load(&zero);
        f.load(&one);
        let source = sim::testing::box_of(&f.chain, 0);
        let to_one = f.chain.trees()[1].clone();
        let mined = f.chain.transfer(&[source], &to_one);
        let stale = f.chain.transfer(&[source], &to_one);
        let txs: Arc<[UnconfirmedTransaction]> = vec![stale.to_unconfirmed()].into();

        let block = f.chain.mine_txs(vec![mined]);
        let snapshot = Event::MempoolSnapshot { txs, observed_tip: block };
        let indexed = Event::BlockApplied(f.chain.oracle().indexed_block(block.height));
        let outputs = if held {
            assert!(f.engine.apply(snapshot).expect("no error").is_empty(), "held");
            f.engine.apply(indexed).expect("extends the tip")
        } else {
            f.engine.apply(indexed).expect("extends the tip");
            f.engine.apply(snapshot).expect("no error")
        };
        assert!(
            !outputs.iter().any(|o| matches!(o, Output::Mempool { .. })),
            "held: {held}, {:?}",
            kinds(&outputs)
        );
        assert!(f.engine.mempool.overlay(&zero).is_empty());
        assert!(f.engine.mempool.overlay(&one).is_empty());
    }
}
