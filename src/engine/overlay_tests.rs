//! The overlay's pure functions on hand-built mempools. The random-history properties, and
//! agreement with the oracle, are in `mempool_tests.rs`.

use std::{cell::RefCell, collections::HashSet};

use super::*;
use crate::{
    sim::testing::{digest, tx, utxo},
    types::{HexBytes, ergo::Token},
};

const OWNER: u8 = 7;
const OTHER: u8 = 8;
const TOKEN: u8 = 9;

fn owner() -> TrackingKey {
    TrackingKey::Tree(HexBytes(vec![OWNER]))
}

fn order(confirmed: &[u8], mempool: &[UnconfirmedTransaction]) -> Vec<u8> {
    let confirmed: HashSet<BoxId> = confirmed.iter().map(|&n| digest(n)).collect();
    pending_order(mempool, |b| confirmed.contains(&b.id))
        .iter()
        .map(|&i| mempool[i].id.0[0])
        .collect()
}

/// Confirmed boxes 1 and 2. Tx 0x30 spends 1 into 0x31; tx 0x10 spends 0x31; tx 0x20
/// spends 2.
fn family() -> Vec<UnconfirmedTransaction> {
    vec![
        tx(0x30, vec![utxo(1, 0, OWNER, &[])], vec![utxo(0x31, 0x30, OWNER, &[])]),
        tx(0x10, vec![utxo(0x31, 0x30, OWNER, &[])], vec![utxo(0x11, 0x10, OWNER, &[])]),
        tx(0x20, vec![utxo(2, 0, OWNER, &[])], vec![utxo(0x21, 0x20, OWNER, &[])]),
    ]
}

#[test]
fn parents_come_first_then_the_smallest_tx_id() {
    let mut mempool = family();
    assert_eq!(order(&[1, 2], &mempool), vec![0x20, 0x30, 0x10]);
    mempool.reverse();
    assert_eq!(order(&[1, 2], &mempool), vec![0x20, 0x30, 0x10], "input order does not matter");
}

#[test]
fn a_tx_with_a_missing_parent_is_left_out_with_its_descendants() {
    let mut mempool = family();
    mempool.push(tx(0x01, vec![utxo(0x11, 0x10, OWNER, &[])], vec![]));
    assert_eq!(order(&[1, 2], &mempool), vec![0x20, 0x30, 0x10, 0x01]);

    let parent = mempool.remove(0);
    assert_eq!(order(&[1, 2], &mempool), vec![0x20]);

    // The parent returns, and so do its descendants.
    mempool.push(parent);
    assert_eq!(order(&[1, 2], &mempool), vec![0x20, 0x30, 0x10, 0x01]);
}

#[test]
fn a_tx_spending_a_box_no_longer_unspent_is_left_out_with_its_descendants() {
    // A block spent box 1, but the snapshot still lists 0x30.
    assert_eq!(order(&[2], &family()), vec![0x20]);
}

#[test]
fn both_sides_of_a_double_spend_are_included() {
    let mempool = vec![
        tx(0x41, vec![utxo(1, 0, OWNER, &[])], vec![]),
        tx(0x40, vec![utxo(1, 0, OWNER, &[])], vec![]),
    ];
    assert_eq!(order(&[1], &mempool), vec![0x40, 0x41]);
}

#[test]
fn a_child_spending_two_outputs_of_its_parent_waits_for_it_once() {
    let mempool = vec![
        tx(
            0x10,
            vec![utxo(1, 0, OWNER, &[])],
            vec![utxo(0x11, 0x10, OWNER, &[]), utxo(0x12, 0x10, OWNER, &[])],
        ),
        tx(0x01, vec![utxo(0x11, 0x10, OWNER, &[]), utxo(0x12, 0x10, OWNER, &[])], vec![]),
    ];
    assert_eq!(order(&[1], &mempool), vec![0x10, 0x01]);
}

#[test]
fn a_repeated_tx_counts_once() {
    let mut mempool = family();
    mempool.push(mempool[0].clone());
    assert_eq!(order(&[1, 2], &mempool), vec![0x20, 0x30, 0x10]);
}

/// The source vouches only for inputs no listed tx creates; the others follow their parent.
#[test]
fn only_inputs_no_listed_tx_creates_are_checked_against_the_confirmed_state() {
    let asked = RefCell::new(Vec::new());
    let mempool = family();
    pending_order(&mempool, |b| {
        asked.borrow_mut().push(b.id.0[0]);
        true
    });
    let mut asked = asked.into_inner();
    asked.sort();
    assert_eq!(asked, vec![1, 2], "0x31 is created by 0x30, so it is never asked about");
}

#[test]
fn a_keys_overlay_lists_only_txs_and_boxes_matching_it() {
    let mempool = vec![
        // Spends a box of OWNER, pays OWNER and OTHER; the OTHER box holds the token twice.
        tx(
            0x50,
            vec![utxo(1, 0, OWNER, &[])],
            vec![utxo(0x51, 0x50, OWNER, &[]), utxo(0x52, 0x50, OTHER, &[(TOKEN, 1), (TOKEN, 2)])],
        ),
        // Touches only OTHER.
        tx(0x60, vec![utxo(2, 0, OTHER, &[])], vec![utxo(0x61, 0x60, OTHER, &[])]),
    ];
    let order = pending_order(&mempool, |_| true);

    assert_eq!(
        overlay(&mempool, &order, &owner()),
        vec![PendingTx {
            tx_id: digest(0x50),
            created: vec![Arc::new(utxo(0x51, 0x50, OWNER, &[]))],
            spent: vec![digest(1)],
        }]
    );

    let token = TrackingKey::Token(digest(TOKEN));
    let holders = overlay(&mempool, &order, &token);
    assert_eq!(holders.len(), 1);
    assert_eq!(holders[0].created.len(), 1, "a box holding the token twice is listed once");
    let keys: Vec<TrackingKey> = keys_of(&mempool[0].outputs[1]).collect();
    assert_eq!(keys, vec![TrackingKey::Tree(HexBytes(vec![OTHER])), token]);
}

#[test]
fn hashes_tell_overlays_apart() {
    let mempool = family();
    let order = pending_order(&mempool, |_| true);
    let full = overlay(&mempool, &order, &owner());
    assert_eq!(full.len(), 3);
    assert_eq!(hash(&full), hash(&full.clone()));
    assert_ne!(hash(&full), hash(&[]));
    assert_ne!(hash(&full), hash(&full[..2]));

    let mut reordered = full.clone();
    reordered.swap(0, 1);
    assert_ne!(hash(&full), hash(&reordered));

    // Moving a box from one side to the other changes the hash.
    let mut moved = full.clone();
    let spent = moved[0].spent.pop().unwrap();
    moved[0]
        .created
        .push(Arc::new(utxo(spent.0[0], 0, OWNER, &[])));
    assert_ne!(hash(&full), hash(&moved));
}

#[test]
fn pending_totals_count_each_side_over_the_whole_overlay() {
    // 0x30 moves box 1 to 0x31, which 0x10 spends into 0x11: 0x31 is on both sides.
    let mut mempool = family();
    mempool[1].outputs[0].tokens = vec![Token { id: digest(TOKEN), amount: 4 }];
    let order = pending_order(&mempool, |_| true);
    let totals = pending_totals(&mempool, &order, &owner());
    assert_eq!(totals.incoming.nano_ergs, 3, "0x31, 0x11 and 0x21");
    assert_eq!(totals.outgoing.nano_ergs, 3, "1, 0x31 and 2");
    assert_eq!(totals.incoming.tokens, BTreeMap::from([(digest(TOKEN), 4)]));
    assert!(totals.outgoing.tokens.is_empty());

    // Txs left out of the overlay count for nothing.
    let order = pending_order(&mempool, |b| b.id != digest(1));
    let totals = pending_totals(&mempool, &order, &owner());
    assert_eq!((totals.incoming.nano_ergs, totals.outgoing.nano_ergs), (1, 1));
    assert!(totals.incoming.tokens.is_empty());
}
