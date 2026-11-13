//! The mempool overlay as pure functions: which pending txs can be valid on top of the
//! confirmed state, in which order, and how each one touches a key.
//!
//! The oracle's `pending_order` / `overlay` (`src/sim/oracle.rs`) is the reference
//! implementation; these must give the same answers, without its quadratic scan.

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    sync::Arc,
};

use blake2::{Blake2b256, Digest as _};

use crate::{
    engine::{PendingTx, Totals, TrackingKey},
    types::{
        Digest, HashDigest,
        ergo::{BoxId, TxId, UTxO, UnconfirmedTransaction},
    },
};

/// The hash of one key's overlay. Two overlays with the same hash are treated as equal.
pub type OverlayHash = HashDigest;

/// The mempool txs that can be valid on top of the confirmed state, as indexes into `txs`, in
/// canonical order.
///
/// An input created by a tx of `txs` is available once that tx is included. Any other input
/// is available if `is_confirmed` says so; it is asked only about those. A tx is included when
/// all its inputs are available, so a tx whose parent is left out goes with all its
/// descendants. Both sides of a double spend are included if both can be.
///
/// The order is topological, taking the smallest tx id among the ready txs at each step
/// (Kahn's algorithm), so it depends only on the set of txs, not on their order in `txs`. A
/// repeated tx id counts once.
pub fn pending_order(
    txs: &[UnconfirmedTransaction],
    is_confirmed: impl Fn(&UTxO) -> bool,
) -> Vec<usize> {
    let mut first: HashSet<TxId> = HashSet::with_capacity(txs.len());
    let live: Vec<bool> = txs.iter().map(|tx| first.insert(tx.id)).collect();
    let mut creator: HashMap<BoxId, usize> = HashMap::new();
    for (i, tx) in txs.iter().enumerate().filter(|(i, _)| live[*i]) {
        creator.extend(tx.outputs.iter().map(|o| (o.id, i)));
    }

    // Per tx, its inputs still waiting for a pending parent; `None` once one can never be
    // available.
    let mut waiting: Vec<Option<usize>> = vec![Some(0); txs.len()];
    let mut children: Vec<Vec<usize>> = vec![Vec::new(); txs.len()];
    let mut ready: BTreeMap<TxId, usize> = BTreeMap::new();
    for (i, tx) in txs.iter().enumerate().filter(|(i, _)| live[*i]) {
        for input in &tx.inputs {
            match creator.get(&input.id) {
                // A tx cannot spend its own output; such an input is never available.
                Some(&parent) if parent == i => waiting[i] = None,
                Some(&parent) => {
                    children[parent].push(i);
                    waiting[i] = waiting[i].map(|n| n + 1);
                }
                None if is_confirmed(input) => {}
                None => waiting[i] = None,
            }
        }
        if waiting[i] == Some(0) {
            ready.insert(tx.id, i);
        }
    }

    let mut order = Vec::new();
    while let Some((_, i)) = ready.pop_first() {
        order.push(i);
        for &child in &children[i] {
            if let Some(n) = &mut waiting[child] {
                *n -= 1;
                if *n == 0 {
                    ready.insert(txs[child].id, child);
                }
            }
        }
    }
    order
}

/// A hash of an overlay: its tx ids and the matching box ids on both sides. Box contents are
/// fixed by their ids, so they are not hashed.
pub fn hash(overlay: &[PendingTx]) -> OverlayHash {
    let mut hasher = Blake2b256::new();
    let len = |n: usize| (n as u64).to_le_bytes();
    hasher.update(len(overlay.len()));
    for tx in overlay {
        hasher.update(tx.tx_id.0);
        hasher.update(len(tx.created.len()));
        for b in &tx.created {
            hasher.update(b.id.0);
        }
        hasher.update(len(tx.spent.len()));
        for id in &tx.spent {
            hasher.update(id.0);
        }
    }
    Digest(hasher.finalize().into())
}

/// How `tx` touches `key`, or `None` if it creates and spends no matching box.
pub fn touch(tx: &UnconfirmedTransaction, key: &TrackingKey) -> Option<PendingTx> {
    let created: Vec<Arc<UTxO>> = tx
        .outputs
        .iter()
        .filter(|b| key.matches(b))
        .map(|b| Arc::new(b.clone()))
        .collect();
    let spent: Vec<BoxId> = tx
        .inputs
        .iter()
        .filter(|b| key.matches(b))
        .map(|b| b.id)
        .collect();
    (!created.is_empty() || !spent.is_empty()).then_some(PendingTx { tx_id: tx.id, created, spent })
}

/// `key`'s overlay over the txs of `order` (from [`pending_order`]): the pending txs that
/// create or spend a box matching the key, each with only its matching boxes, in that order.
pub fn overlay(
    txs: &[UnconfirmedTransaction],
    order: &[usize],
    key: &TrackingKey,
) -> Vec<PendingTx> {
    order.iter().filter_map(|&i| touch(&txs[i], key)).collect()
}

/// A key's pending ERG and tokens.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PendingTotals {
    /// Over the boxes matching the key that pending txs create.
    pub incoming: Totals,
    /// Over the boxes matching the key that pending txs spend.
    pub outgoing: Totals,
}

/// `key`'s pending totals over the txs of `order`. A box created and spent within the overlay
/// counts on both sides, so `incoming - outgoing` is the net change.
pub fn pending_totals(
    txs: &[UnconfirmedTransaction],
    order: &[usize],
    key: &TrackingKey,
) -> PendingTotals {
    let mut totals = PendingTotals::default();
    for tx in order.iter().map(|&i| &txs[i]) {
        for b in tx.outputs.iter().filter(|b| key.matches(b)) {
            totals.incoming.add(b);
        }
        for b in tx.inputs.iter().filter(|b| key.matches(b)) {
            totals.outgoing.add(b);
        }
    }
    totals
}

/// The keys a box matches: its tree, and each token it holds, once each.
pub fn keys_of(utxo: &UTxO) -> impl Iterator<Item = TrackingKey> + '_ {
    let tokens = utxo
        .tokens
        .iter()
        .enumerate()
        .filter(|(i, t)| !utxo.tokens[..*i].iter().any(|u| u.id == t.id))
        .map(|(_, t)| TrackingKey::Token(t.id));
    std::iter::once(TrackingKey::Tree(utxo.ergo_tree.clone())).chain(tokens)
}

#[cfg(test)]
#[path = "overlay_tests.rs"]
mod tests;
