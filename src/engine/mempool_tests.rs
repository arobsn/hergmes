//! Layer 2: the mempool overlay properties, over random `SimChain`
//! histories with the oracle as reference, plus the engine's emission rules.
//!
//! Partial confirmation and reorg returns need block handling, so they come with step 5.

use std::collections::{HashMap, HashSet};

use proptest::prelude::*;

use super::*;
use crate::{
    engine::{
        PendingTx,
        overlay::{keys_of, pending_order},
    },
    sim::{
        self, Chain, SimConfig, strategy,
        testing::{digest, new_chain, sample_history, tx, utxo},
    },
    types::{
        Digest, HexBytes,
        ergo::{BoxId, TxId},
    },
};

const CASES: u32 = 10_000;

/// A deterministic Fisher-Yates shuffle (SplitMix64).
fn shuffled<T: Clone>(items: &[T], mut seed: u64) -> Vec<T> {
    let mut next = || {
        seed = seed.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = seed;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    };
    let mut items = items.to_vec();
    for i in (1..items.len()).rev() {
        items.swap(i, (next() % (i as u64 + 1)) as usize);
    }
    items
}

fn ids(txs: &[UnconfirmedTransaction], order: &[usize]) -> Vec<TxId> {
    order.iter().map(|&i| txs[i].id).collect()
}

/// The reference pending totals: the oracle's overlay, summed side by side.
fn reference_totals(mempool: &[UnconfirmedTransaction], overlay: &[PendingTx]) -> PendingTotals {
    let inputs: HashMap<BoxId, &UTxO> = mempool
        .iter()
        .flat_map(|tx| tx.inputs.iter().map(|i| (i.id, i)))
        .collect();
    PendingTotals {
        incoming: sim::totals(overlay.iter().flat_map(|p| p.created.iter().map(|b| &**b))),
        outgoing: sim::totals(
            overlay
                .iter()
                .flat_map(|p| p.spent.iter().map(|id| inputs[id])),
        ),
    }
}

/// Every pending spend refers to a confirmed box or to one created by an earlier tx of the
/// same overlay (handshake step 4).
fn assert_spends_known_boxes(overlay: &[PendingTx], confirmed: &HashSet<BoxId>) {
    let mut created: HashSet<BoxId> = HashSet::new();
    for tx in overlay {
        for id in &tx.spent {
            assert!(
                confirmed.contains(id) || created.contains(id),
                "{} spends {id}, which the client does not have",
                tx.tx_id
            );
        }
        created.extend(tx.created.iter().map(|b| b.id));
    }
}

/// Checks one state of a history: the snapshot `chain.mempool()` at `chain.tip()`, fed to a
/// `Mempool` tracking `tracked`, whose previous overlays were `sent`.
fn check_state(
    chain: &Chain,
    mempool: &mut Mempool,
    tracked: &[TrackingKey],
    sent: &mut BTreeMap<TrackingKey, Vec<PendingTx>>,
    seed: u64,
) {
    let snapshot: Arc<[UnconfirmedTransaction]> = chain.mempool().into();
    let tip = chain.tip();
    let confirmed: HashSet<BoxId> = chain.utxo().values().map(|b| b.id).collect();
    let is_confirmed = |b: &UTxO| confirmed.contains(&b.id);

    // The same canonical order as the oracle; and since the source drops what cannot be valid,
    // every listed tx is in it.
    let reference_order = sim::pending_order(&confirmed, &snapshot);
    let overlays: BTreeMap<TrackingKey, Vec<PendingTx>> = chain
        .keys()
        .into_iter()
        .map(|key| {
            let overlay = sim::filter_overlay(&reference_order, &key);
            (key, overlay)
        })
        .collect();
    let order = pending_order(&snapshot, is_confirmed);
    let reference: Vec<TxId> = reference_order.iter().map(|tx| tx.id).collect();
    assert_eq!(ids(&snapshot, &order), reference);
    assert_eq!(order.len(), snapshot.len());

    // Pure: the input order does not matter.
    let reordered: Arc<[UnconfirmedTransaction]> = shuffled(&snapshot, seed).into();
    assert_eq!(ids(&reordered, &pending_order(&reordered, is_confirmed)), reference);

    // Observation skew: a snapshot from another block changes nothing.
    let before = mempool.latest().map(|(txs, at)| (txs.to_vec(), at));
    let elsewhere = [
        BlockRef { height: tip.height + 1, header_id: digest(0xee) },
        BlockRef { height: tip.height, header_id: digest(0xee) },
        BlockRef { height: tip.height.saturating_sub(1), header_id: tip.header_id },
    ];
    for observed in elsewhere {
        assert!(
            mempool
                .apply_snapshot(snapshot.clone(), observed, Some(tip), is_confirmed)
                .is_empty()
        );
    }
    assert!(
        mempool
            .apply_snapshot(snapshot.clone(), tip, None, is_confirmed)
            .is_empty()
    );
    assert_eq!(mempool.latest().map(|(txs, at)| (txs.to_vec(), at)), before);

    // Minimal emission: a message for exactly the tracked keys whose overlay changed, in key
    // order, carrying the new overlay.
    let outputs = mempool.apply_snapshot(snapshot.clone(), tip, Some(tip), is_confirmed);
    let mut expected = Vec::new();
    for key in tracked {
        let overlay = &overlays[key];
        let previous = sent.get_mut(key).expect("tracked from the start");
        if overlay != previous {
            expected.push(Output::Mempool {
                key: key.clone(),
                tip_height: tip.height,
                pending: overlay.clone(),
            });
            *previous = overlay.clone();
        }
    }
    assert_eq!(outputs, expected);

    // Idempotent: the same snapshot again, in any order, emits nothing.
    assert!(
        mempool
            .apply_snapshot(snapshot.clone(), tip, Some(tip), is_confirmed)
            .is_empty()
    );
    assert!(
        mempool
            .apply_snapshot(reordered, tip, Some(tip), is_confirmed)
            .is_empty()
    );

    // Every key, tracked or not, answers like the oracle.
    for (key, reference) in &overlays {
        let overlay = mempool.overlay(key);
        assert_eq!(&overlay, reference);
        assert_spends_known_boxes(&overlay, &confirmed);
        assert_eq!(mempool.pending_totals(key), reference_totals(&snapshot, reference));
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(CASES))]

    /// After every op of a random history, the engine's overlay for the new snapshot equals
    /// the oracle's for every key, and it emits exactly the changed overlays of tracked keys.
    #[test]
    fn overlays_follow_the_oracle_and_emit_only_changes(
        ops in strategy::history(30, 3),
        mask in any::<u64>(),
        seed in any::<u64>(),
    ) {
        let mut chain = new_chain();
        let keys = chain.keys();
        // In key order, the order the engine sends messages in.
        let mut tracked: Vec<TrackingKey> = keys
            .iter()
            .enumerate()
            .filter(|(i, _)| mask >> (i % 64) & 1 == 1)
            .map(|(_, k)| k.clone())
            .collect();
        tracked.sort();
        let mut mempool = Mempool::default();
        for key in &tracked {
            mempool.track(key.clone());
        }
        let mut sent: BTreeMap<TrackingKey, Vec<PendingTx>> =
            tracked.iter().map(|k| (k.clone(), Vec::new())).collect();

        for (step, op) in ops.iter().enumerate() {
            chain.apply(op);
            check_state(&chain, &mut mempool, &tracked, &mut sent, seed ^ step as u64);
        }
    }

    /// For any confirmed set and any subset of the snapshot, the order and every overlay equal
    /// the oracle's: txs whose input is not confirmed (a block spent it) or whose parent is
    /// missing go with all their descendants, and come back with them.
    #[test]
    fn any_confirmed_set_and_missing_txs_match_the_oracle(
        ops in strategy::history(30, 3),
        unconfirm in prop::collection::vec(any::<prop::sample::Index>(), 0..4),
        missing in prop::collection::vec(any::<prop::sample::Index>(), 0..3),
        seed in any::<u64>(),
    ) {
        let chain = Chain::from_ops(SimConfig::default(), &ops);
        let full = chain.mempool();
        let mut confirmed: HashSet<BoxId> = chain.utxo().values().map(|b| b.id).collect();
        // Spend some confirmed inputs of pending txs, as a block the snapshot has not seen.
        let inputs: Vec<BoxId> = full
            .iter()
            .flat_map(|tx| tx.inputs.iter().map(|i| i.id))
            .filter(|id| confirmed.contains(id))
            .collect();
        if !inputs.is_empty() {
            for index in &unconfirm {
                confirmed.remove(&inputs[index.index(inputs.len())]);
            }
        }
        let mut snapshot = full.clone();
        for index in &missing {
            if !snapshot.is_empty() {
                snapshot.remove(index.index(snapshot.len()));
            }
        }
        let snapshot = shuffled(&snapshot, seed);

        let is_confirmed = |b: &UTxO| confirmed.contains(&b.id);
        let order = pending_order(&snapshot, is_confirmed);
        let reference: Vec<TxId> = sim::pending_order(&confirmed, &snapshot)
            .iter()
            .map(|tx| tx.id)
            .collect();
        prop_assert_eq!(ids(&snapshot, &order), reference);

        // Nothing included spends an output of a tx that is not.
        let included: HashSet<TxId> = ids(&snapshot, &order).into_iter().collect();
        let left_out: HashSet<BoxId> = full
            .iter()
            .filter(|tx| !included.contains(&tx.id))
            .flat_map(|tx| tx.outputs.iter().map(|o| o.id))
            .collect();
        for &i in &order {
            prop_assert!(snapshot[i].inputs.iter().all(|b| !left_out.contains(&b.id)));
        }

        for key in chain.keys() {
            let overlay = overlay::overlay(&snapshot, &order, &key);
            prop_assert_eq!(&overlay, &sim::overlay(&confirmed, &snapshot, &key));
            assert_spends_known_boxes(&overlay, &confirmed);
        }

        // The txs come back with the full snapshot and the confirmed set at the tip.
        let confirmed: HashSet<BoxId> = chain.utxo().values().map(|b| b.id).collect();
        let order = pending_order(&full, |b| confirmed.contains(&b.id));
        prop_assert_eq!(order.len(), full.len());
    }
}

/// The properties are only useful if random histories reach the cases that break overlays.
#[test]
fn random_histories_reach_the_hard_overlay_cases() {
    let mut chain = new_chain();
    let mut mempool = Mempool::default();
    for key in chain.keys() {
        mempool.track(key);
    }
    let mut seen = HashSet::new();
    for op in sample_history(400) {
        chain.apply(&op);
        let snapshot: Arc<[UnconfirmedTransaction]> = chain.mempool().into();
        let confirmed: HashSet<BoxId> = chain.utxo().values().map(|b| b.id).collect();
        let is_confirmed = |b: &UTxO| confirmed.contains(&b.id);
        let tip = chain.tip();
        for output in mempool.apply_snapshot(snapshot.clone(), tip, Some(tip), is_confirmed) {
            let Output::Mempool { pending, .. } = output else {
                panic!("only mempool messages");
            };
            if pending.is_empty() {
                seen.insert("emptied overlay sent");
            }
        }

        let order = pending_order(&snapshot, is_confirmed);
        let mut claimed = HashSet::new();
        if !order
            .iter()
            .flat_map(|&i| &snapshot[i].inputs)
            .all(|b| claimed.insert(b.id))
        {
            seen.insert("both sides of a double spend in the overlay");
        }
        for key in chain.keys() {
            let overlay = overlay::overlay(&snapshot, &order, &key);
            let created: HashSet<BoxId> = overlay
                .iter()
                .flat_map(|tx| tx.created.iter().map(|b| b.id))
                .collect();
            if overlay
                .iter()
                .flat_map(|tx| &tx.spent)
                .any(|id| created.contains(id))
            {
                seen.insert("chained txs in one key's overlay");
            }
        }
        if order
            .iter()
            .flat_map(|&i| &snapshot[i].outputs)
            .any(|b| keys_of(b).count() >= 3)
        {
            seen.insert("pending box matching three keys");
        }
        // Without a tx that has a pending child, the child goes too.
        let created_by: HashMap<BoxId, usize> = snapshot
            .iter()
            .enumerate()
            .flat_map(|(i, tx)| tx.outputs.iter().map(move |o| (o.id, i)))
            .collect();
        if let Some(parent) = snapshot
            .iter()
            .flat_map(|tx| &tx.inputs)
            .find_map(|b| created_by.get(&b.id))
        {
            let mut without = snapshot.to_vec();
            without.remove(*parent);
            if pending_order(&without, is_confirmed).len() + 1 < order.len() {
                seen.insert("descendants left out with a missing parent");
            }
        }
    }

    for feature in [
        "emptied overlay sent",
        "both sides of a double spend in the overlay",
        "chained txs in one key's overlay",
        "pending box matching three keys",
        "descendants left out with a missing parent",
    ] {
        assert!(seen.contains(feature), "no random history reached: {feature}");
    }
}

// --- The engine's emission rules on hand-built snapshots ---

const OWNER: u8 = 7;
const OTHER: u8 = 8;

fn key(owner: u8) -> TrackingKey {
    TrackingKey::Tree(HexBytes(vec![owner]))
}

fn at(height: u32) -> BlockRef {
    BlockRef { height, header_id: digest(height as u8) }
}

/// Box 1 of OWNER and box 2 of OTHER are confirmed.
fn confirmed(b: &UTxO) -> bool {
    [digest(1), digest(2)].contains(&b.id)
}

fn pay(id: u8, from: (u8, u8), to: u8) -> UnconfirmedTransaction {
    tx(id, vec![utxo(from.0, 0, from.1, &[])], vec![utxo(id + 1, id, to, &[])])
}

fn snapshot(txs: Vec<UnconfirmedTransaction>) -> Arc<[UnconfirmedTransaction]> {
    txs.into()
}

#[test]
fn an_empty_overlay_is_not_sent_until_it_was_not_empty() {
    let mut mempool = Mempool::default();
    mempool.track(key(OWNER));
    assert!(
        mempool
            .apply_snapshot(snapshot(vec![]), at(1), Some(at(1)), confirmed)
            .is_empty()
    );

    let spend = pay(0x10, (1, OWNER), OTHER);
    let outputs = mempool.apply_snapshot(snapshot(vec![spend]), at(1), Some(at(1)), confirmed);
    assert_eq!(outputs.len(), 1);
    let Output::Mempool { key: k, tip_height, pending } = &outputs[0] else {
        panic!("a mempool message");
    };
    assert_eq!((k, *tip_height, pending.len()), (&key(OWNER), 1, 1));

    // The tx leaves: the key's subscribers must drop it.
    let outputs = mempool.apply_snapshot(snapshot(vec![]), at(1), Some(at(1)), confirmed);
    assert_eq!(outputs, vec![Output::Mempool { key: key(OWNER), tip_height: 1, pending: vec![] }]);
}

#[test]
fn a_change_touching_only_untracked_keys_sends_nothing() {
    let mut mempool = Mempool::default();
    mempool.track(key(OWNER));
    let other = pay(0x20, (2, OTHER), OTHER);
    assert!(
        mempool
            .apply_snapshot(snapshot(vec![other]), at(1), Some(at(1)), confirmed)
            .is_empty()
    );
    assert_eq!(mempool.overlay(&key(OTHER)).len(), 1, "untracked keys are answered on demand");
}

#[test]
fn a_tracked_key_starts_from_the_current_overlay() {
    let mut mempool = Mempool::default();
    let spend = pay(0x10, (1, OWNER), OTHER);
    mempool.apply_snapshot(snapshot(vec![spend.clone()]), at(1), Some(at(1)), confirmed);

    // Its subscribers get the overlay with their catch-up, so the same snapshot sends nothing.
    mempool.track(key(OWNER));
    assert!(mempool.is_tracked(&key(OWNER)));
    assert!(
        mempool
            .apply_snapshot(snapshot(vec![spend]), at(1), Some(at(1)), confirmed)
            .is_empty()
    );

    mempool.untrack(&key(OWNER));
    assert!(
        mempool
            .apply_snapshot(snapshot(vec![]), at(1), Some(at(1)), confirmed)
            .is_empty()
    );
}

#[test]
fn a_snapshot_from_another_block_is_ignored() {
    let mut mempool = Mempool::default();
    mempool.track(key(OWNER));
    let spend = snapshot(vec![pay(0x10, (1, OWNER), OTHER)]);
    for observed in [at(2), at(0), BlockRef { height: 1, header_id: digest(0xee) }] {
        assert!(
            mempool
                .apply_snapshot(spend.clone(), observed, Some(at(1)), confirmed)
                .is_empty()
        );
    }
    assert!(
        mempool
            .apply_snapshot(spend.clone(), at(1), None, confirmed)
            .is_empty()
    );
    assert!(mempool.latest().is_none());
    assert!(mempool.overlay(&key(OWNER)).is_empty());

    assert_eq!(
        mempool
            .apply_snapshot(spend, at(1), Some(at(1)), confirmed)
            .len(),
        1
    );
    assert_eq!(mempool.latest().map(|(_, tip)| tip), Some(at(1)));
}

/// No cap: a key with many pending txs gets all of them, in canonical order.
#[test]
fn a_long_overlay_is_sent_whole() {
    let mut mempool = Mempool::default();
    mempool.track(key(OWNER));
    // A chain of 300 txs, each spending the previous one's output, from confirmed box 1.
    let mut txs = Vec::new();
    let mut from = utxo(1, 0, OWNER, &[]);
    for n in 0..300u32 {
        let id = Digest(blake2_of(n));
        let mut output = utxo(0, 0, OWNER, &[]);
        output.id = Digest(blake2_of(n + 1_000_000));
        output.transaction_id = id;
        txs.push(UnconfirmedTransaction { id, inputs: vec![from], outputs: vec![output.clone()] });
        from = output;
    }
    let expected: Vec<TxId> = txs.iter().map(|tx| tx.id).collect();

    let outputs =
        mempool.apply_snapshot(snapshot(shuffled(&txs, 7)), at(1), Some(at(1)), confirmed);
    let [Output::Mempool { pending, .. }] = &outputs[..] else {
        panic!("one message, got {} outputs", outputs.len());
    };
    let sent: Vec<TxId> = pending.iter().map(|tx| tx.tx_id).collect();
    assert_eq!(sent, expected, "every tx, parents first");
}

fn blake2_of(n: u32) -> [u8; 32] {
    use blake2::{Blake2b256, Digest as _};
    Blake2b256::digest(n.to_le_bytes()).into()
}

#[test]
fn a_box_matching_several_tracked_keys_is_shared_between_their_overlays() {
    let mut mempool = Mempool::default();
    let token = TrackingKey::Token(digest(9));
    mempool.track(key(OWNER));
    mempool.track(token.clone());
    let mint = tx(0x10, vec![utxo(1, 0, OWNER, &[])], vec![utxo(0x11, 0x10, OWNER, &[(9, 1)])]);
    assert_eq!(keys_of(&mint.outputs[0]).count(), 2);

    let outputs = mempool.apply_snapshot(snapshot(vec![mint]), at(1), Some(at(1)), confirmed);
    let created: Vec<&Arc<UTxO>> = outputs
        .iter()
        .map(|o| match o {
            Output::Mempool { pending, .. } => &pending[0].created[0],
            other => panic!("unexpected {other:?}"),
        })
        .collect();
    assert_eq!(created.len(), 2);
    assert!(Arc::ptr_eq(created[0], created[1]), "one copy of the box");
    assert_eq!(mempool.overlay(&token)[0].spent, Vec::<BoxId>::new());
}
