//! The engine: per-key unspent id sets and running totals, the block delta ring buffer, the
//! mempool overlays and balances, driven by events and queries one at a time.

use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque},
    ops::Bound,
    sync::Arc,
};

use crate::{
    engine::{
        Balance, BlockDelta, BlockRef, BoxKey, EngineError, Event, KeyDelta, KeyId, Mempool,
        Output, PageError, Query, Reply, ResyncReason, Spend, StoredBox, Totals, TrackingKey,
        overlay::{self, PendingDelta},
    },
    types::ergo::{BoxId, HeaderId, IndexedBlock, IndexedBox, UTxO},
};

/// How far back the engine can serve, and how long it keeps idle keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Config {
    /// N: how many blocks of deltas the ring buffer keeps. It bounds replay on reconnect,
    /// rollbacks, and pages pinned to an earlier block.
    pub window: u32,
    /// G: how many blocks a key with no subscribers is kept after its last activity.
    pub grace: u32,
}

impl Default for Config {
    fn default() -> Self {
        Self { window: 720, grace: 5 }
    }
}

/// A tracked key's state.
#[derive(Debug, Clone)]
struct KeyState {
    key: TrackingKey,
    /// The key's unspent boxes at the tip.
    boxes: BTreeSet<BoxKey>,
    totals: Totals,
    /// The block the key was loaded at. Ring entries up to it have no share for the key.
    since: BlockRef,
    /// The engine's block count when the key was loaded (see [`BlockDelta::seq`]).
    loaded_seq: u64,
    subscribers: u32,
    /// The tip height at the key's last activity: its load, a subscribe or unsubscribe, or a
    /// read.
    last_activity: u32,
    /// The pending delta of the last `balance` sent. The confirmed half is `totals`.
    pending_sent: PendingDelta,
}

#[derive(Debug, Clone)]
pub struct Engine {
    config: Config,
    tip: Option<BlockRef>,
    /// Blocks applied so far.
    seq: u64,
    next_id: u32,
    /// Tracked keys, in the order messages go out in.
    ids: BTreeMap<TrackingKey, KeyId>,
    keys: HashMap<KeyId, KeyState>,
    /// Every box a tracked key holds, once per key, with its inclusion height.
    reverse: BTreeMap<(BoxId, KeyId), u32>,
    /// The last blocks of the main chain, oldest first, ending at the tip.
    ring: VecDeque<BlockDelta>,
    /// Reverted blocks, kept to roll a client on their branch back to the fork point.
    orphans: HashMap<HeaderId, BlockDelta>,
    mempool: Mempool,
}

impl Engine {
    pub fn new(config: Config) -> Self {
        assert!(config.window >= 1, "the ring buffer keeps at least the tip");
        Self {
            config,
            tip: None,
            seq: 0,
            next_id: 0,
            ids: BTreeMap::new(),
            keys: HashMap::new(),
            reverse: BTreeMap::new(),
            ring: VecDeque::new(),
            orphans: HashMap::new(),
            mempool: Mempool::default(),
        }
    }

    pub fn tip(&self) -> Option<BlockRef> {
        self.tip
    }

    pub fn is_tracked(&self, key: &TrackingKey) -> bool {
        self.ids.contains_key(key)
    }

    /// Applies one event. Within it, messages go out as `Block` / `Rollback` /
    /// `ResyncRequired` / `LoadRejected`, then `Mempool`, then `Balance`, each in key order.
    pub fn apply(&mut self, event: Event) -> Result<Vec<Output>, EngineError> {
        Ok(match event {
            Event::BlockApplied(block) => self.block_applied(&block)?,
            Event::BlockReverted(block) => self.block_reverted(block)?,
            Event::MempoolSnapshot { txs, observed_tip } => {
                let confirmed = Confirmed::new(&self.ids, &self.reverse);
                let mut outputs = self
                    .mempool
                    .apply_snapshot(txs, observed_tip, self.tip, |b| confirmed.check(b));
                self.push_balances(&BTreeMap::new(), &mut outputs);
                outputs
            }
            Event::KeyLoaded { key, as_of, boxes } => self.key_loaded(key, as_of, boxes),
            Event::Subscribed(key) => match self.touch(&key) {
                Some(state) => {
                    state.subscribers += 1;
                    Vec::new()
                }
                None => vec![Output::ResyncRequired { key, reason: ResyncReason::NotTracked }],
            },
            Event::Unsubscribed(key) => {
                if let Some(state) = self.touch(&key) {
                    state.subscribers = state.subscribers.saturating_sub(1);
                }
                Vec::new()
            }
        })
    }

    /// Answers one query. Reads of a tracked key count as its activity.
    pub fn query(&mut self, query: Query) -> Reply {
        match query {
            Query::Page { key, at, after, limit } => Reply::Page(self.page(&key, at, after, limit)),
            Query::Overlay(key) => Reply::Overlay(self.mempool.overlay(&key)),
            Query::Pending(key) => Reply::Pending(self.mempool.pending_delta(&key)),
            Query::Balance(key) => {
                self.touch(&key);
                Reply::Balance(self.balance(&key))
            }
            Query::Tip => Reply::Tip(self.tip),
            Query::MempoolSnapshot => Reply::MempoolSnapshot(self.mempool.latest()),
            Query::CatchUp { key, from } => Reply::CatchUp(self.catch_up(key, from)),
        }
    }

    // --- Blocks ---

    fn block_applied(&mut self, block: &IndexedBlock) -> Result<Vec<Output>, EngineError> {
        let block_ref = BlockRef::from(&block.header);
        if let Some(tip) = self.tip
            && (block.header.parent_id != tip.header_id || block_ref.height != tip.height + 1)
        {
            return Err(EngineError::NotChild { block: block_ref, tip: self.tip });
        }
        self.seq += 1;
        let mut delta = BlockDelta {
            block: block_ref,
            parent_id: block.header.parent_id,
            seq: self.seq,
            boxes: HashMap::new(),
            keys: HashMap::new(),
        };
        let mut before: BTreeMap<KeyId, Totals> = BTreeMap::new();
        let mut mined = HashSet::new();
        let mut spent = HashSet::new();

        // Txs in order, so a box created and spent within the block nets out of each delta.
        for tx in &block.transactions {
            mined.insert(tx.id);
            for input in &tx.inputs {
                let id = input.utxo.id;
                spent.insert(id);
                for (key_id, height) in self.take_holders(&id) {
                    let state = self.keys.get_mut(&key_id).expect("a holder is tracked");
                    before.entry(key_id).or_insert_with(|| state.totals.clone());
                    state
                        .boxes
                        .remove(&BoxKey { inclusion_height: height, box_id: id });
                    state.totals.sub(&input.utxo);
                    let key_delta = delta.keys.entry(key_id).or_default();
                    match key_delta.created.iter().position(|c| *c == id) {
                        Some(i) => {
                            key_delta.created.remove(i);
                        }
                        None => {
                            key_delta.spent.push(Spend { box_id: id, tx_id: tx.id });
                            delta.boxes.entry(id).or_insert_with(|| StoredBox {
                                inclusion_height: height,
                                utxo: Arc::new(input.utxo.clone()),
                            });
                        }
                    }
                }
            }
            for output in &tx.outputs {
                let holders: Vec<KeyId> = overlay::keys_of(&output.utxo)
                    .filter_map(|k| self.ids.get(&k).copied())
                    .collect();
                if holders.is_empty() {
                    continue;
                }
                let id = output.utxo.id;
                let stored = StoredBox {
                    inclusion_height: block_ref.height,
                    utxo: Arc::new(output.utxo.clone()),
                };
                for key_id in holders {
                    let state = self.keys.get_mut(&key_id).expect("an id is tracked");
                    before.entry(key_id).or_insert_with(|| state.totals.clone());
                    state
                        .boxes
                        .insert(BoxKey { inclusion_height: block_ref.height, box_id: id });
                    state.totals.add(&output.utxo);
                    self.reverse.insert((id, key_id), block_ref.height);
                    delta.keys.entry(key_id).or_default().created.push(id);
                }
                delta.boxes.insert(id, stored);
            }
        }
        // Boxes created and spent within the block are in no delta.
        delta.keys.retain(|_, d| !d.is_empty());
        let referenced: HashSet<BoxId> = delta
            .keys
            .values()
            .flat_map(|d| {
                d.created
                    .iter()
                    .copied()
                    .chain(d.spent.iter().map(|s| s.box_id))
            })
            .collect();
        delta.boxes.retain(|id, _| referenced.contains(id));

        let mut outputs = Vec::new();
        for (key, key_id) in &self.ids {
            let key_delta = delta.keys.get(key_id);
            outputs.push(Output::Block {
                key: key.clone(),
                block: block_ref,
                parent_id: delta.parent_id,
                created: key_delta.map_or_else(Vec::new, |d| created_boxes(&delta, d)),
                spent: key_delta.map_or_else(Vec::new, |d| d.spent.clone()),
            });
        }

        self.orphans.remove(&block_ref.header_id);
        self.ring.push_back(delta);
        while self.ring.len() > self.config.window as usize {
            self.ring.pop_front();
        }
        self.tip = Some(block_ref);
        let floor = self.floor();
        self.orphans.retain(|_, o| o.block.height > floor);

        // Untrack idle keys before deciding the overlays: their boxes are no longer checked.
        let idle: Vec<TrackingKey> = self
            .keys
            .values()
            .filter(|s| {
                s.subscribers == 0
                    && block_ref.height.saturating_sub(s.last_activity) > self.config.grace
            })
            .map(|s| s.key.clone())
            .collect();
        for key in idle {
            self.untrack(&key);
            before.retain(|id, _| self.keys.contains_key(id));
        }

        let confirmed = Confirmed::new(&self.ids, &self.reverse);
        outputs.extend(
            self.mempool
                .block_applied(block_ref, &mined, &spent, |b| confirmed.check(b)),
        );
        self.push_balances(&before, &mut outputs);
        Ok(outputs)
    }

    fn block_reverted(&mut self, block: BlockRef) -> Result<Vec<Output>, EngineError> {
        if self.tip != Some(block) {
            return Err(EngineError::NotTip { block, tip: self.tip });
        }
        let Some(delta) = self.ring.pop_back() else {
            // The ring is exhausted: the fork is deeper than the window. Every key's state is
            // on the old chain and cannot be undone.
            let mut outputs = Vec::new();
            for key in self.ids.keys() {
                let reason = ResyncReason::ForkBeyondWindow;
                outputs.push(Output::ResyncRequired { key: key.clone(), reason });
            }
            let keys: Vec<TrackingKey> = self.ids.keys().cloned().collect();
            for key in keys {
                self.untrack(&key);
            }
            self.tip = None;
            self.mempool.reset();
            self.orphans.clear();
            return Ok(outputs);
        };
        debug_assert_eq!(delta.block, block);

        let mut outputs = Vec::new();
        let mut dropped = Vec::new();
        let mut before: BTreeMap<KeyId, Totals> = BTreeMap::new();
        for (key, &key_id) in &self.ids {
            let state = self.keys.get_mut(&key_id).expect("an id is tracked");
            if state.since == block {
                // Loaded at this block: its share of the block was never recorded.
                let reason = ResyncReason::ForkBeyondWindow;
                outputs.push(Output::ResyncRequired { key: key.clone(), reason });
                dropped.push(key.clone());
                continue;
            }
            let key_delta = delta.keys.get(&key_id);
            if let Some(d) = key_delta {
                before.insert(key_id, state.totals.clone());
                for id in &d.created {
                    let stored = &delta.boxes[id];
                    state
                        .boxes
                        .remove(&BoxKey { inclusion_height: block.height, box_id: *id });
                    state.totals.sub(&stored.utxo);
                    self.reverse.remove(&(*id, key_id));
                }
                for spend in &d.spent {
                    let stored = &delta.boxes[&spend.box_id];
                    let box_key =
                        BoxKey { inclusion_height: stored.inclusion_height, box_id: spend.box_id };
                    state.boxes.insert(box_key);
                    state.totals.add(&stored.utxo);
                    self.reverse
                        .insert((spend.box_id, key_id), stored.inclusion_height);
                }
            }
            outputs.push(rollback(key, &delta, key_delta));
        }
        for key in &dropped {
            self.untrack(key);
        }

        let parent = delta.parent();
        self.tip = Some(parent);
        self.orphans.insert(block.header_id, delta);
        outputs.extend(self.mempool.block_reverted(parent));
        self.push_balances(&before, &mut outputs);
        Ok(outputs)
    }

    // --- Keys ---

    fn key_loaded(
        &mut self,
        key: TrackingKey,
        as_of: BlockRef,
        boxes: Vec<IndexedBox>,
    ) -> Vec<Output> {
        if self.tip != Some(as_of) {
            return vec![Output::LoadRejected { key, as_of }];
        }
        if self.touch(&key).is_some() {
            // Another load of a tracked key, from a racing loader: the engine's state is newer.
            return Vec::new();
        }
        let key_id = KeyId(self.next_id);
        self.next_id += 1;
        let mut state = KeyState {
            key: key.clone(),
            boxes: BTreeSet::new(),
            totals: Totals::default(),
            since: as_of,
            loaded_seq: self.seq,
            subscribers: 0,
            last_activity: as_of.height,
            pending_sent: PendingDelta::default(),
        };
        for b in &boxes {
            debug_assert!(key.matches(&b.utxo), "the loader checks every box");
            let box_key = BoxKey { inclusion_height: b.inclusion_height, box_id: b.utxo.id };
            if state.boxes.insert(box_key) {
                state.totals.add(&b.utxo);
                self.reverse.insert((b.utxo.id, key_id), b.inclusion_height);
            }
        }
        self.ids.insert(key.clone(), key_id);
        self.keys.insert(key_id, state);

        // The key's boxes are now checked against its set, which can leave txs out.
        let confirmed = Confirmed::new(&self.ids, &self.reverse);
        let mut outputs = self.mempool.redecide(|b| confirmed.check(b));
        self.mempool.track(key.clone());
        let delta = self
            .mempool
            .tracked_delta(&key)
            .cloned()
            .unwrap_or_default();
        self.keys
            .get_mut(&key_id)
            .expect("just inserted")
            .pending_sent = delta;
        self.push_balances(&BTreeMap::new(), &mut outputs);
        outputs
    }

    /// Records activity on a tracked key, and returns its state.
    fn touch(&mut self, key: &TrackingKey) -> Option<&mut KeyState> {
        let height = self.tip.map_or(0, |t| t.height);
        let state = self.keys.get_mut(self.ids.get(key)?)?;
        state.last_activity = height;
        Some(state)
    }

    /// Stops tracking `key`, with its boxes and its deltas.
    fn untrack(&mut self, key: &TrackingKey) {
        let Some(key_id) = self.ids.remove(key) else {
            return;
        };
        let state = self.keys.remove(&key_id).expect("an id is tracked");
        for b in &state.boxes {
            self.reverse.remove(&(b.box_id, key_id));
        }
        for delta in self.ring.iter_mut().chain(self.orphans.values_mut()) {
            if delta.keys.remove(&key_id).is_some() {
                let referenced: HashSet<BoxId> = delta
                    .keys
                    .values()
                    .flat_map(|d| {
                        d.created
                            .iter()
                            .copied()
                            .chain(d.spent.iter().map(|s| s.box_id))
                    })
                    .collect();
                delta.boxes.retain(|id, _| referenced.contains(id));
            }
        }
        self.mempool.untrack(key);
    }

    /// Removes and returns the keys holding box `id`, with its inclusion height.
    fn take_holders(&mut self, id: &BoxId) -> Vec<(KeyId, u32)> {
        let holders: Vec<(KeyId, u32)> = self
            .reverse
            .range((*id, KeyId::MIN)..=(*id, KeyId::MAX))
            .map(|(&(_, k), &h)| (k, h))
            .collect();
        for (key_id, _) in &holders {
            self.reverse.remove(&(*id, *key_id));
        }
        holders
    }

    // --- Balances ---

    fn balance(&self, key: &TrackingKey) -> Option<Balance> {
        let state = &self.keys[self.ids.get(key)?];
        Some(Balance {
            as_of: self.tip?,
            confirmed: state.totals.clone(),
            pending: self.mempool.tracked_delta(key).cloned().unwrap_or_default(),
        })
    }

    /// Appends a `Balance` for each tracked key whose totals changed in this step (`before`
    /// holds the totals of the keys the step touched, as they were) or whose pending delta
    /// differs from the one sent.
    fn push_balances(&mut self, before: &BTreeMap<KeyId, Totals>, outputs: &mut Vec<Output>) {
        let Some(tip) = self.tip else {
            return;
        };
        for (key, key_id) in &self.ids {
            let state = self.keys.get_mut(key_id).expect("an id is tracked");
            let pending = self.mempool.tracked_delta(key).cloned().unwrap_or_default();
            let totals_changed = before.get(key_id).is_some_and(|t| *t != state.totals);
            if totals_changed || pending != state.pending_sent {
                state.pending_sent = pending.clone();
                let balance = Balance { as_of: tip, confirmed: state.totals.clone(), pending };
                outputs.push(Output::Balance { key: key.clone(), balance });
            }
        }
    }

    // --- Reads ---

    /// The lowest height whose state the engine can still serve: the parent of the oldest
    /// block in the ring, or the tip when the ring is empty.
    fn floor(&self) -> u32 {
        match self.ring.front() {
            Some(oldest) => oldest.block.height - 1,
            None => self.tip.map_or(0, |t| t.height),
        }
    }

    /// Whether `block` is on the main chain within the window.
    fn on_main(&self, block: BlockRef) -> bool {
        if self.tip == Some(block) {
            return true;
        }
        let Some(oldest) = self.ring.front() else {
            return false;
        };
        if block.height + 1 == oldest.block.height {
            return oldest.parent_id == block.header_id;
        }
        block
            .height
            .checked_sub(oldest.block.height)
            .and_then(|i| self.ring.get(i as usize))
            .is_some_and(|d| d.block == block)
    }

    /// Ring entries above `height`, oldest first.
    fn ring_above(&self, height: u32) -> impl Iterator<Item = &BlockDelta> {
        self.ring.iter().filter(move |d| d.block.height > height)
    }

    fn page(
        &mut self,
        key: &TrackingKey,
        at: BlockRef,
        after: Option<BoxKey>,
        limit: usize,
    ) -> Result<Vec<BoxKey>, PageError> {
        let since = self.touch(key).ok_or(PageError::NotTracked)?.since;
        let key_id = self.ids[key];
        let state = &self.keys[&key_id];
        if at.height < since.height || !self.on_main(at) {
            return Err(PageError::CursorExpired);
        }
        let lower = after.map_or(Bound::Unbounded, Bound::Excluded);

        // The set at `at` is the tip's boxes that existed then (included at or below it), and
        // the boxes spent since that existed then.
        let mut restored = BTreeSet::new();
        for delta in self.ring_above(at.height) {
            for spend in delta.keys.get(&key_id).into_iter().flat_map(|d| &d.spent) {
                let height = delta.boxes[&spend.box_id].inclusion_height;
                if height <= at.height {
                    restored.insert(BoxKey { inclusion_height: height, box_id: spend.box_id });
                }
            }
        }
        let current = state
            .boxes
            .range((lower, Bound::Unbounded))
            .take_while(|b| b.inclusion_height <= at.height);
        let restored = restored.range((lower, Bound::Unbounded));
        Ok(merge(current, restored).take(limit).collect())
    }

    fn catch_up(&mut self, key: TrackingKey, from: Option<BlockRef>) -> Vec<Output> {
        let Some(state) = self.touch(&key) else {
            return vec![Output::ResyncRequired { key, reason: ResyncReason::NotTracked }];
        };
        let (since, loaded_seq) = (state.since, state.loaded_seq);
        let key_id = self.ids[&key];
        let mut outputs = Vec::new();
        if let Some(from) = from {
            // Roll back through orphaned blocks to the main chain, then replay.
            let mut cursor = from;
            let mut rollbacks = Vec::new();
            while !self.on_main(cursor) {
                let reason = if cursor.height < self.floor() {
                    ResyncReason::OutsideWindow
                } else {
                    match self.orphans.get(&cursor.header_id) {
                        Some(orphan) if orphan.seq > loaded_seq => {
                            rollbacks.push(orphan);
                            cursor = orphan.parent();
                            continue;
                        }
                        Some(_) => ResyncReason::NotTracked,
                        None => ResyncReason::ForkBeyondWindow,
                    }
                };
                return vec![Output::ResyncRequired { key, reason }];
            }
            if cursor.height < since.height {
                return vec![Output::ResyncRequired { key, reason: ResyncReason::NotTracked }];
            }
            for orphan in rollbacks {
                outputs.push(rollback(&key, orphan, orphan.keys.get(&key_id)));
            }
            for delta in self.ring_above(cursor.height) {
                let key_delta = delta.keys.get(&key_id);
                outputs.push(Output::Block {
                    key: key.clone(),
                    block: delta.block,
                    parent_id: delta.parent_id,
                    created: key_delta.map_or_else(Vec::new, |d| created_boxes(delta, d)),
                    spent: key_delta.map_or_else(Vec::new, |d| d.spent.clone()),
                });
            }
            let tip_height = self.tip.map_or(0, |t| t.height);
            let pending = self.mempool.overlay(&key);
            outputs.push(Output::Mempool { key: key.clone(), tip_height, pending });
        }
        if let Some(balance) = self.balance(&key) {
            outputs.push(Output::Balance { key, balance });
        }
        outputs
    }
}

/// Decides whether a pending tx's input counts as confirmed at the engine's tip: a box a
/// tracked key would hold counts only if one does; any other box counts, on the source's word
/// (see `overlay::pending_order`).
struct Confirmed<'a> {
    ids: &'a BTreeMap<TrackingKey, KeyId>,
    reverse: &'a BTreeMap<(BoxId, KeyId), u32>,
}

impl<'a> Confirmed<'a> {
    fn new(
        ids: &'a BTreeMap<TrackingKey, KeyId>,
        reverse: &'a BTreeMap<(BoxId, KeyId), u32>,
    ) -> Self {
        Self { ids, reverse }
    }

    fn check(&self, b: &UTxO) -> bool {
        let held = self
            .reverse
            .range((b.id, KeyId::MIN)..=(b.id, KeyId::MAX))
            .next()
            .is_some();
        held || !overlay::keys_of(b).any(|k| self.ids.contains_key(&k))
    }
}

fn created_boxes(delta: &BlockDelta, key_delta: &KeyDelta) -> Vec<Arc<UTxO>> {
    key_delta
        .created
        .iter()
        .map(|id| delta.boxes[id].utxo.clone())
        .collect()
}

/// The `Rollback` of `delta` for `key`: its sides swapped.
fn rollback(key: &TrackingKey, delta: &BlockDelta, key_delta: Option<&KeyDelta>) -> Output {
    let (created, spent) = match key_delta {
        Some(d) => (
            d.spent
                .iter()
                .map(|s| delta.boxes[&s.box_id].utxo.clone())
                .collect(),
            d.created
                .iter()
                .map(|id| Spend { box_id: *id, tx_id: delta.boxes[id].utxo.transaction_id })
                .collect(),
        ),
        None => (Vec::new(), Vec::new()),
    };
    Output::Rollback {
        key: key.clone(),
        block: delta.block,
        parent_id: delta.parent_id,
        created,
        spent,
    }
}

/// Merges two ascending iterators of distinct keys.
fn merge<'a>(
    a: impl Iterator<Item = &'a BoxKey>,
    b: impl Iterator<Item = &'a BoxKey>,
) -> impl Iterator<Item = BoxKey> {
    let (mut a, mut b) = (a.peekable(), b.peekable());
    std::iter::from_fn(move || match (a.peek(), b.peek()) {
        (Some(x), Some(y)) if x <= y => a.next().copied(),
        (Some(_), Some(_)) => b.next().copied(),
        (Some(_), None) => a.next().copied(),
        (None, _) => b.next().copied(),
    })
}

#[cfg(test)]
#[path = "state_tests.rs"]
mod tests;
