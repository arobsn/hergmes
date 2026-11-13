//! Checks of the simulator itself: the model is consistent, the generator reaches the
//! hard cases, and the indexer view matches the oracle once it has caught up.

use std::collections::{BTreeMap, HashSet};

use proptest::prelude::*;

use super::*;
use crate::sim::{
    strategy,
    testing::{ALL, box_of, new_chain, sample_history},
    totals,
};

#[test]
fn genesis_splits_tokens_over_different_trees() {
    let chain = new_chain();
    let oracle = chain.oracle();
    let height = chain.height();
    for (name, emission, parts) in GENESIS_TOKENS {
        let key = TrackingKey::Token(token_id(name));
        let boxes = oracle.unspent(&key, height);
        assert_eq!(boxes.len(), parts.len(), "{name}");
        let owners: HashSet<_> = boxes.values().map(|b| &b.ergo_tree).collect();
        assert_eq!(owners.len(), parts.len(), "{name}'s boxes have different owners");
        assert_eq!(oracle.totals(&key, height).tokens[&token_id(name)], emission);
    }

    let emissions: Vec<u64> = GENESIS_TOKENS.iter().map(|t| t.1).collect();
    assert!(emissions.contains(&1) && emissions.contains(&100) && emissions.contains(&101));
    assert!(
        oracle.utxo(height).values().any(|b| b.tokens.len() >= 2),
        "some box matches its tree and two token keys"
    );
}

#[test]
fn the_same_ops_give_the_same_chain() {
    let ops = sample_history(80);
    let a = Chain::from_ops(SimConfig::default(), &ops);
    let b = Chain::from_ops(SimConfig::default(), &ops);
    assert_eq!(a.tip(), b.tip());
    assert_eq!(a.utxo(), b.utxo());
    assert_eq!(a.mempool(), b.mempool());
    assert_eq!(a.indexed_tip(), b.indexed_tip());
}

/// The generator is only useful if random histories reach the cases that break relays.
#[test]
fn random_histories_reach_the_hard_cases() {
    let mut chain = new_chain();
    let mut seen = HashSet::new();
    for op in sample_history(400) {
        let listed_before: HashSet<TxId> = chain.visible_pool().iter().map(|tx| tx.id).collect();
        chain.apply(&op);
        let visible = chain.visible_pool();
        if matches!(op, SimOp::MempoolConflict { .. })
            && !listed_before
                .iter()
                .all(|id| visible.iter().any(|tx| tx.id == *id))
        {
            seen.insert("pending tx replaced by a double spend");
        }
        if visible.len() < chain.pool().len() {
            seen.insert("pending tx with an unresolved input");
        }
        let pending_outputs: HashSet<BoxId> = visible
            .iter()
            .flat_map(|tx| tx.outputs.iter().map(|o| o.id))
            .collect();
        if visible
            .iter()
            .flat_map(|tx| &tx.inputs)
            .any(|i| pending_outputs.contains(&i.id))
        {
            seen.insert("chained pending txs");
        }
        if read_through_recovery(&mut chain) > 0 {
            seen.insert("indexer recovering");
        }
        if chain.indexed_tip().height < chain.height() {
            seen.insert("indexer lagging");
        }
    }

    let main_txs: HashSet<TxId> = chain
        .branch(&chain.tip().header_id)
        .iter()
        .flat_map(|b| b.txs.iter().map(|tx| tx.id))
        .collect();
    for block in chain.blocks.values() {
        if !chain.on_main(&block.header.id) {
            seen.insert("orphaned block");
            if block.txs.iter().any(|tx| main_txs.contains(&tx.id)) {
                seen.insert("tx mined again after a reorg");
            }
        }
        let mut created = HashSet::new();
        for tx in block.txs.iter().filter(|tx| !tx.inputs.is_empty()) {
            if tx.inputs.iter().any(|i| created.contains(&i.id)) {
                seen.insert("box created and spent in one block");
            }
            created.extend(tx.outputs.iter().map(|o| o.id));
            if !tx.minted().is_empty() {
                seen.insert("token minted");
            }
            if tx.outputs.iter().any(|o| o.tokens.len() >= 2) {
                seen.insert("new box matching three keys");
            }
            for token in tx.inputs.iter().chain(&tx.outputs).flat_map(|b| &b.tokens) {
                let holders = |boxes: &[Arc<UTxO>]| -> Vec<(ErgoTree, u64)> {
                    boxes
                        .iter()
                        .flat_map(|b| {
                            b.tokens
                                .iter()
                                .filter(|t| t.id == token.id)
                                .map(|t| (b.ergo_tree.clone(), t.amount))
                        })
                        .collect()
                };
                let (before, after) = (holders(&tx.inputs), holders(&tx.outputs));
                if before.len() < after.len() {
                    seen.insert("token split");
                }
                if before.len() > after.len() && !after.is_empty() {
                    seen.insert("token merged");
                }
                let units = |h: &[(ErgoTree, u64)]| h.iter().map(|(_, a)| a).sum::<u64>();
                if units(&after) < units(&before) {
                    seen.insert("token burned");
                }
                let owners = |h: &[(ErgoTree, u64)]| h.iter().map(|(t, _)| t.clone()).collect();
                let (from, to): (HashSet<ErgoTree>, HashSet<ErgoTree>) =
                    (owners(&before), owners(&after));
                if !from.is_empty() && !to.is_empty() && from != to {
                    seen.insert("token changed owner");
                }
            }
        }
    }

    for feature in [
        "pending tx with an unresolved input",
        "chained pending txs",
        "pending tx replaced by a double spend",
        "indexer recovering",
        "indexer lagging",
        "orphaned block",
        "tx mined again after a reorg",
        "box created and spent in one block",
        "token minted",
        "new box matching three keys",
        "token split",
        "token merged",
        "token burned",
        "token changed owner",
    ] {
        assert!(seen.contains(feature), "no random history reached: {feature}");
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn the_simulator_agrees_with_the_oracle(ops in strategy::history(40, 4)) {
        let mut chain = new_chain();
        let genesis_value: u64 = chain.utxo().values().map(|b| b.value).sum();
        for op in &ops {
            chain.apply(op);
            check_against_oracle(&chain, genesis_value);
            read_through_recovery(&mut chain);
            check_against_oracle(&chain, genesis_value);
        }
    }
}

/// Retries indexer reads until the indexer answers, like an adapter would. Recovery ends
/// only through reads. Returns how many reads got a `503`.
fn read_through_recovery(chain: &mut Chain) -> u32 {
    let mut unavailable = 0;
    while chain.indexer_gate().is_err() {
        unavailable += 1;
        assert!(unavailable <= chain.config().recovery_reads, "recovery never ends");
    }
    unavailable
}

fn check_against_oracle(chain: &Chain, genesis_value: u64) {
    let oracle = chain.oracle();
    let tip = chain.tip();
    assert_eq!(oracle.tip(), tip);
    let utxo = oracle.utxo(tip.height);
    assert_eq!(chain.utxo(), utxo, "the node's UTXO set differs from a replay");

    let all = totals(utxo.values().map(|b| &**b));
    assert_eq!(all.nano_ergs, genesis_value, "ERG is conserved");
    for (id, amount) in &all.tokens {
        let emission = chain.node.tokens[id];
        assert!(*amount <= emission, "more of {id} than was minted");
        let holders = utxo
            .values()
            .filter(|b| b.tokens.iter().any(|t| t.id == *id))
            .count();
        assert!(holders as u64 <= emission, "{id} is in more boxes than its emission");
    }

    // The mempool lists only txs the node can resolve, and none that are confirmed.
    let unspent: HashSet<BoxId> = utxo.values().map(|b| b.id).collect();
    let pending_outputs = chain.pool_outputs();
    let confirmed: HashSet<TxId> = chain
        .branch(&tip.header_id)
        .iter()
        .flat_map(|b| b.txs.iter().map(|tx| tx.id))
        .collect();
    let listed = chain.mempool();
    let mut claimed = HashSet::new();
    assert!(
        listed
            .iter()
            .flat_map(|tx| &tx.inputs)
            .all(|i| claimed.insert(i.id)),
        "the node never lists both sides of a double spend"
    );
    for tx in listed {
        assert!(
            tx.inputs
                .iter()
                .all(|i| unspent.contains(&i.id) || pending_outputs.contains(&i.id))
        );
        assert!(!confirmed.contains(&tx.id), "{} is both pending and confirmed", tx.id);
    }

    if chain.is_recovering() {
        return;
    }
    let indexed = chain.indexed_tip();
    assert_eq!(chain.header_at(indexed.height), Some(indexed), "indexer off the selected chain");
    assert!(tip.height - indexed.height <= chain.indexer_lag());
    let heights = chain.indexed_height();
    assert_eq!((heights.indexed_height, heights.full_height), (indexed.height, tip.height));

    let at_indexed = oracle.utxo(indexed.height);
    for key in chain.keys() {
        let expected: Vec<BoxKey> = at_indexed
            .iter()
            .filter(|(_, b)| key.matches(b))
            .map(|(k, _)| *k)
            .collect();
        let mut served: Vec<BoxKey> = chain
            .indexed_unspent(|b| key.matches(b), ALL)
            .unwrap()
            .iter()
            .map(|b| BoxKey { inclusion_height: b.inclusion_height, box_id: b.utxo.id })
            .collect();
        served.sort();
        assert_eq!(served, expected, "indexer's unspent set of {key:?}");

        if let TrackingKey::Tree(tree) = &key {
            let balance = chain.confirmed_balance(tree);
            let expected = totals(at_indexed.values().filter(|b| key.matches(b)).map(|b| &**b));
            assert_eq!(balance.nano_ergs, expected.nano_ergs);
            let tokens: BTreeMap<_, _> = balance
                .tokens
                .iter()
                .map(|t| (t.token_id, t.amount))
                .collect();
            assert_eq!(tokens, expected.tokens);
        }
    }

    let block = chain
        .indexed_block(&indexed.header_id)
        .expect("the indexed tip");
    for tx in &block.transactions {
        assert!(
            tx.inputs
                .iter()
                .all(|i| i.inclusion_height <= indexed.height)
        );
        assert!(
            tx.outputs
                .iter()
                .all(|o| o.inclusion_height == indexed.height)
        );
    }
    if indexed != tip {
        assert!(chain.indexed_block(&tip.header_id).is_none(), "the tip is not indexed yet");
    }
}

#[test]
fn a_reorged_tx_returns_to_the_mempool_and_keeps_its_box_ids() {
    let mut chain = new_chain();
    let to = chain.trees()[1].clone();
    let tx = chain.transfer(&[box_of(&chain, 0)], &to);
    let first = chain.mine_txs(vec![tx.clone()]).height;

    assert_eq!(chain.orphan(1), 1);
    assert_eq!(chain.mempool(), vec![tx.to_unconfirmed()]);

    // The other branch mines it one block later.
    chain.mine_txs(vec![]);
    chain.mine_txs(vec![tx.clone()]);
    assert!(chain.mempool().is_empty());
    let moved = BoxKey { inclusion_height: first + 1, box_id: tx.outputs[0].id };
    assert!(chain.utxo().contains_key(&moved));
}

#[test]
fn orphan_never_reaches_genesis() {
    let mut chain = new_chain();
    chain.mine(0, 1, 1);
    assert_eq!(chain.orphan(5), 1);
    assert_eq!(chain.height(), chain.config().start_height);
    assert_eq!(chain.reorg(3, 0, 1), 0);
    assert_eq!(chain.height(), chain.config().start_height);
}

#[test]
fn a_block_can_spend_a_box_it_creates() {
    let mut chain = new_chain();
    let (one, two) = (chain.trees()[1].clone(), chain.trees()[2].clone());
    let parent = chain.transfer(&[box_of(&chain, 0)], &one);
    let child = chain.transfer(&[parent.outputs[0].id], &two);
    chain.mine_txs(vec![parent.clone(), child.clone()]);

    let ids: HashSet<BoxId> = chain.utxo().values().map(|b| b.id).collect();
    assert!(!ids.contains(&parent.outputs[0].id));
    assert!(ids.contains(&child.outputs[0].id));
}

#[test]
#[should_panic(expected = "not unspent")]
fn a_block_cannot_spend_a_spent_box() {
    let mut chain = new_chain();
    let (one, two) = (chain.trees()[1].clone(), chain.trees()[2].clone());
    let source = box_of(&chain, 0);
    let first = chain.transfer(&[source], &one);
    let second = chain.transfer(&[source], &two);
    chain.mine_txs(vec![first]);
    chain.mine_txs(vec![second]);
}

#[test]
fn a_child_of_a_dropped_tx_is_hidden_but_a_grandchild_is_not() {
    let mut chain = new_chain();
    let trees = chain.trees().to_vec();
    let parent = chain.transfer(&[box_of(&chain, 0)], &trees[1]);
    let child = chain.transfer(&[parent.outputs[0].id], &trees[2]);
    let grandchild = chain.transfer(&[child.outputs[0].id], &trees[3]);
    for tx in [&parent, &child, &grandchild] {
        chain.submit(tx.clone());
    }
    assert_eq!(chain.mempool().len(), 3);

    // The child's input is gone, but the node still resolves the grandchild's input from
    // the child, which stays in its pool until the next block (ergo#2248).
    assert!(chain.drop_tx(&parent.id));
    let listed: Vec<TxId> = chain.mempool().iter().map(|tx| tx.id).collect();
    assert_eq!(listed, vec![grandchild.id]);

    // The cleanup the next block triggers drops both.
    chain.mine(0, 0, 0);
    assert!(chain.pool().is_empty());
}

#[test]
fn a_double_spend_replaces_the_pending_tx_and_hides_its_child() {
    let mut chain = new_chain();
    let trees = chain.trees().to_vec();
    let source = box_of(&chain, 0);
    let first = chain.transfer(&[source], &trees[1]);
    let child = chain.transfer(&[first.outputs[0].id], &trees[2]);
    let second = chain.transfer(&[source], &trees[3]);
    chain.submit(first.clone());
    chain.submit(child.clone());
    chain.submit(second.clone());

    let listed: Vec<TxId> = chain.mempool().iter().map(|tx| tx.id).collect();
    assert_eq!(listed, vec![second.id]);
    assert!(chain.pool().iter().any(|tx| tx.id == child.id), "the child stays, unresolvable");
}

#[test]
fn a_block_removes_pending_double_spends_of_its_txs() {
    let mut chain = new_chain();
    let trees = chain.trees().to_vec();
    let source = box_of(&chain, 0);
    let pending = chain.transfer(&[source], &trees[1]);
    let child = chain.transfer(&[pending.outputs[0].id], &trees[3]);
    chain.submit(pending);
    chain.submit(child.clone());
    let mined = chain.transfer(&[source], &trees[2]);
    chain.mine_txs(vec![mined]);

    // The child stays, unresolvable and hidden, until the next block's cleanup.
    let pool: Vec<TxId> = chain.pool().iter().map(|tx| tx.id).collect();
    assert_eq!(pool, vec![child.id]);
    assert!(chain.mempool().is_empty());
    chain.mine(0, 0, 0);
    assert!(chain.pool().is_empty());
}

#[test]
fn the_mempool_clock_moves_forward_on_blocks_pool_changes_and_restarts() {
    let mut chain = new_chain();
    let start = chain.info().last_mempool_update;
    chain.mine(0, 2, 1);
    let mined = chain.info().last_mempool_update;
    assert!(mined > start, "every block republishes the mempool");

    chain.mempool_add(1).expect("a pending tx");
    let added = chain.info().last_mempool_update;
    assert!(added > mined);

    chain.restart_node();
    assert!(chain.info().last_mempool_update > added, "it is the launch time");
    assert!(chain.mempool().is_empty());
}

#[test]
#[should_panic(expected = "ERG is not conserved")]
fn txs_conserve_erg() {
    let mut chain = new_chain();
    let source = box_of(&chain, 0);
    let to = chain.trees()[1].clone();
    let value = chain.find_box(&source).unwrap().value;
    chain.tx(&[source], vec![OutputSpec { ergo_tree: to, value: value + 1, tokens: vec![] }]);
}
