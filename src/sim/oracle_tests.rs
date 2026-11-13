//! The oracle's answers over a branch, and the reference overlay's semantics.

use std::collections::HashSet;

use super::*;
use crate::{
    sim::{
        Chain, SimConfig,
        testing::{box_of, new_chain, sample_history},
    },
    types::{
        Digest, HashDigest, HexBytes,
        ergo::{NonMandatoryRegisters, Token},
    },
};

#[test]
fn answers_at_any_height_of_the_branch() {
    let mut chain = new_chain();
    let trees = chain.trees().to_vec();
    let source = box_of(&chain, 0);
    let tx = chain.transfer(&[source], &trees[1]);
    let start = chain.height();
    chain.mine_txs(vec![tx.clone()]);
    let oracle = chain.oracle();

    let (zero, one) = (TrackingKey::Tree(trees[0].clone()), TrackingKey::Tree(trees[1].clone()));
    let ids = |key, height| -> HashSet<BoxId> {
        oracle.unspent(key, height).values().map(|b| b.id).collect()
    };
    assert!(ids(&zero, start).contains(&source));
    assert!(!ids(&zero, start + 1).contains(&source));
    assert!(ids(&one, start + 1).contains(&tx.outputs[0].id));

    let moved = tx.outputs[0].value;
    let before = oracle.totals(&one, start).nano_ergs;
    assert_eq!(oracle.totals(&one, start + 1).nano_ergs, before + moved);
}

#[test]
fn a_reorg_and_more_blocks_leave_earlier_answers_alone() {
    let mut chain = Chain::from_ops(SimConfig::default(), &sample_history(60));
    for seed in 0..4 {
        chain.mine(2, 2, seed);
    }
    let before = chain.oracle();
    let orphaned = chain.tip();
    let fork = orphaned.height - 3;

    chain.reorg(3, 1, 42);
    for seed in 0..3 {
        chain.mine(1, 2, seed);
    }
    let after = chain.oracle();

    let start = chain.config().start_height;
    for height in start..=fork {
        assert_eq!(before.block(height), after.block(height));
        assert_eq!(before.utxo(height), after.utxo(height), "at {height}");
    }
    assert_ne!(before.block(fork + 1), after.block(fork + 1));
    assert_eq!(after.utxo(after.tip().height), chain.utxo());

    // The orphaned branch can still be asked about, e.g. for expired cursors.
    let old = chain.oracle_at(&orphaned.header_id);
    assert_eq!(old.tip(), orphaned);
    assert_eq!(old.utxo(orphaned.height), before.utxo(orphaned.height));
}

#[test]
fn totals_leave_out_tokens_with_no_units() {
    assert_eq!(totals([]), Totals::default());
    let b = utxo(1, 0, 7, &[(9, 3)]);
    let t = totals([&b, &b]);
    assert_eq!(t.nano_ergs, 2);
    assert_eq!(t.tokens.into_iter().collect::<Vec<_>>(), vec![(digest(9), 6)]);
}

fn digest(n: u8) -> HashDigest {
    Digest([n; 32])
}

/// A box with id `n` created by tx `tx`, owned by tree `[owner]`, worth 1 nanoERG.
fn utxo(n: u8, tx: u8, owner: u8, tokens: &[(u8, u64)]) -> UTxO {
    UTxO {
        id: digest(n),
        ergo_tree: HexBytes(vec![owner]),
        creation_height: 1,
        value: 1,
        tokens: tokens
            .iter()
            .map(|&(id, amount)| Token { id: digest(id), amount })
            .collect(),
        registers: NonMandatoryRegisters::default(),
        index: 0,
        transaction_id: digest(tx),
    }
}

fn tx(id: u8, inputs: Vec<UTxO>, outputs: Vec<UTxO>) -> UnconfirmedTransaction {
    UnconfirmedTransaction { id: digest(id), inputs, outputs }
}

fn order(confirmed: &[u8], mempool: &[UnconfirmedTransaction]) -> Vec<u8> {
    let confirmed: HashSet<BoxId> = confirmed.iter().map(|&n| digest(n)).collect();
    pending_order(&confirmed, mempool)
        .iter()
        .map(|tx| tx.id.0[0])
        .collect()
}

const OWNER: u8 = 7;

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
fn a_tx_with_a_missing_parent_is_left_out_with_its_children() {
    let mut mempool = family();
    mempool.push(tx(0x01, vec![utxo(0x11, 0x10, OWNER, &[])], vec![]));
    // The smallest id, but it waits for its grandparent 0x30 and its parent 0x10.
    assert_eq!(order(&[1, 2], &mempool), vec![0x20, 0x30, 0x10, 0x01]);

    // Without 0x30, its child 0x10 and grandchild 0x01 go too.
    mempool.remove(0);
    assert_eq!(order(&[1, 2], &mempool), vec![0x20]);
}

#[test]
fn a_tx_spending_a_box_no_longer_unspent_is_left_out() {
    // A block spent box 1, but the node still lists the tx.
    assert_eq!(order(&[2], &family()), vec![0x20]);
}

#[test]
fn both_sides_of_a_conflict_are_listed() {
    let mempool = vec![
        tx(0x41, vec![utxo(1, 0, OWNER, &[])], vec![]),
        tx(0x40, vec![utxo(1, 0, OWNER, &[])], vec![]),
    ];
    assert_eq!(order(&[1], &mempool), vec![0x40, 0x41]);
}

#[test]
fn a_keys_overlay_lists_only_txs_and_boxes_matching_it() {
    let (other, token) = (8, 9);
    let mempool = vec![
        // Spends a box of OWNER, pays OWNER and other; the other box holds the token.
        tx(
            0x50,
            vec![utxo(1, 0, OWNER, &[])],
            vec![utxo(0x51, 0x50, OWNER, &[]), utxo(0x52, 0x50, other, &[(token, 1)])],
        ),
        // Touches only other.
        tx(0x60, vec![utxo(2, 0, other, &[])], vec![utxo(0x61, 0x60, other, &[])]),
    ];
    let confirmed: HashSet<BoxId> = [digest(1), digest(2)].into();

    let owner = overlay(&confirmed, &mempool, &TrackingKey::Tree(HexBytes(vec![OWNER])));
    assert_eq!(
        owner,
        vec![PendingTx {
            tx_id: digest(0x50),
            created: vec![Arc::new(utxo(0x51, 0x50, OWNER, &[]))],
            spent: vec![digest(1)],
        }]
    );

    let holders = overlay(&confirmed, &mempool, &TrackingKey::Token(digest(token)));
    assert_eq!(
        holders,
        vec![PendingTx {
            tx_id: digest(0x50),
            created: vec![Arc::new(utxo(0x52, 0x50, other, &[(token, 1)]))],
            spent: vec![],
        }]
    );
}
