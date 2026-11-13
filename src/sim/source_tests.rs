//! `SimChain` through `NodeSource`: the node view, the lagging and recovering indexer, and
//! offset paging over a set that changes between reads.

use std::collections::HashSet;

use reqwest::StatusCode;

use super::*;
use crate::{
    clients::source::MAX_PAGE_LIMIT,
    engine::TrackingKey,
    sim::{
        GENESIS_TOKENS, OutputSpec, SimConfig,
        testing::{ALL, box_of, new_chain, ready},
        token_id,
    },
    types::Digest,
};

fn is_unavailable<T>(result: Result<T, NodeError>) -> bool {
    matches!(result, Err(NodeError::Unavailable { retry_after: Some(_) }))
}

#[test]
fn offset_paging_skips_a_box_spent_between_reads() {
    let mut chain = new_chain();
    let trees = chain.trees().to_vec();
    // Tree 0 starts with two boxes; give it three more, one per block.
    for index in 1..=3 {
        let tx = chain.transfer(&[box_of(&chain, index)], &trees[0]);
        chain.mine_txs(vec![tx]);
    }
    let sim = SimChain::new(chain);
    let before: Vec<BoxId> = ready(sim.unspent_by_ergo_tree(&trees[0], ALL))
        .unwrap()
        .iter()
        .map(|b| b.utxo.id)
        .collect();
    assert_eq!(before.len(), 5);

    // Before the second page read, a block spends the first box of the first page.
    let to = trees[4].clone();
    let mut reads = 0;
    sim.on_call(move |endpoint, chain| {
        if endpoint == Endpoint::UnspentByErgoTree {
            reads += 1;
            if reads == 2 {
                let tx = chain.transfer(&[before[0]], &to);
                chain.mine_txs(vec![tx]);
            }
        }
    });

    let mut seen = HashSet::new();
    for offset in [0, 2, 4] {
        let page = ready(sim.unspent_by_ergo_tree(&trees[0], Page { offset, limit: 2 })).unwrap();
        seen.extend(page.iter().map(|b| b.utxo.id));
    }
    let after: Vec<BoxId> = ready(sim.unspent_by_ergo_tree(&trees[0], ALL))
        .unwrap()
        .iter()
        .map(|b| b.utxo.id)
        .collect();
    let skipped: Vec<BoxId> = after
        .iter()
        .filter(|id| !seen.contains(id))
        .copied()
        .collect();
    assert_eq!(skipped.len(), 1, "the box that slid back into page one was never read");
    assert_eq!(sim.calls(), vec![Endpoint::UnspentByErgoTree; 5]);
}

#[test]
fn pages_follow_the_indexers_global_order() {
    let mut chain = new_chain();
    let trees = chain.trees().to_vec();
    let source = box_of(&chain, 1);
    let value = chain.find_box(&source).unwrap().value;
    let outputs = (0..4)
        .map(|i| OutputSpec {
            ergo_tree: trees[0].clone(),
            value: value / 4 + if i == 0 { value % 4 } else { 0 },
            tokens: vec![],
        })
        .collect();
    // Tokens the source holds are burned.
    let tx = chain.tx(&[source], outputs);
    chain.mine_txs(vec![tx.clone()]);
    let sim = SimChain::new(chain);
    let page = ready(sim.unspent_by_ergo_tree(&trees[0], ALL)).unwrap();
    let ids: Vec<BoxId> = page.iter().map(|b| b.utxo.id).collect();
    // Boxes of one block come in output order, not box id order.
    let outputs: Vec<BoxId> = tx.outputs.iter().map(|o| o.id).collect();
    assert_eq!(&ids[ids.len() - 4..], &outputs[..]);
}

#[test]
fn a_limit_above_the_maximum_is_rejected() {
    let sim = SimChain::new(new_chain());
    let tree = sim.lock().trees()[0].clone();
    let page = Page { offset: 0, limit: MAX_PAGE_LIMIT + 1 };
    match ready(sim.unspent_by_ergo_tree(&tree, page)) {
        Err(NodeError::Api { status, reason, .. }) => {
            assert_eq!(status, StatusCode::BAD_REQUEST);
            assert_eq!(reason, "bad.request");
        }
        other => panic!("expected 400, got {other:?}"),
    }
}

#[test]
fn a_lagging_indexer_serves_its_own_tip() {
    let mut chain = new_chain();
    chain.set_indexer_lag(2);
    for seed in 0..4 {
        chain.mine(1, 2, seed);
    }
    let tip = chain.tip();
    let indexed = chain.indexed_tip();
    let tree = chain.trees()[0].clone();
    let expected = chain
        .oracle()
        .totals(&TrackingKey::Tree(tree.clone()), indexed.height);
    let sim = SimChain::new(chain);

    let heights = ready(sim.indexed_height()).unwrap();
    assert_eq!(heights.full_height, tip.height);
    assert_eq!(heights.indexed_height, tip.height - 2);
    assert!(ready(sim.indexed_block(&tip.header_id)).unwrap().is_none());
    assert!(
        ready(sim.indexed_block(&indexed.header_id))
            .unwrap()
            .is_some()
    );
    assert_eq!(ready(sim.confirmed_balance(&tree)).unwrap().nano_ergs, expected.nano_ergs);
    // The node view is not delayed.
    assert_eq!(ready(sim.info()).unwrap().best_full_header_id, Some(tip.header_id));
}

#[test]
fn orphaning_the_indexed_tip_answers_503_until_the_indexer_recovers() {
    let mut chain = Chain::new(SimConfig { recovery_reads: 2, ..SimConfig::default() });
    for seed in 0..3 {
        chain.mine(0, 2, seed);
    }
    let orphaned = chain.tip();
    chain.reorg(1, 0, 7);
    let tip = chain.tip();
    let token = token_id("nft-a");
    let sim = SimChain::new(chain);

    for _ in 0..2 {
        // `indexedHeight` still answers, with the orphaned tip's height.
        let heights = ready(sim.indexed_height()).unwrap();
        assert_eq!(heights.indexed_height, orphaned.height);
        assert!(is_unavailable(ready(sim.token_info(&token))));
        // Node endpoints keep answering, and don't count as indexer reads.
        assert_eq!(ready(sim.info()).unwrap().best_full_header_id, Some(tip.header_id));
        assert!(ready(sim.mempool()).is_ok());
    }
    assert!(ready(sim.token_info(&token)).unwrap().is_some());
    let heights = ready(sim.indexed_height()).unwrap();
    assert_eq!(heights.indexed_height, heights.full_height);
    assert!(
        ready(sim.indexed_block(&orphaned.header_id))
            .unwrap()
            .is_none()
    );
    assert!(ready(sim.indexed_block(&tip.header_id)).unwrap().is_some());
}

#[test]
fn a_reorg_below_a_lagging_indexer_needs_no_recovery() {
    let mut chain = new_chain();
    chain.set_indexer_lag(2);
    for seed in 0..4 {
        chain.mine(0, 1, seed);
    }
    chain.reorg(1, 0, 3);
    assert!(!chain.is_recovering());
    let sim = SimChain::new(chain);
    assert!(ready(sim.token_info(&token_id("nft-a"))).is_ok());
}

#[test]
fn token_info_follows_the_indexed_chain() {
    let mut chain = new_chain();
    chain.set_indexer_lag(1);
    let source = box_of(&chain, 0);
    let to = chain.trees()[1].clone();
    let mint = chain.mint(&source, 7, &to);
    chain.mine_txs(vec![mint]);
    let sim = SimChain::new(chain);

    for (name, emission, _) in GENESIS_TOKENS {
        let info = ready(sim.token_info(&token_id(name))).unwrap().unwrap();
        assert_eq!(info.emission_amount, emission);
    }
    assert!(ready(sim.token_info(&Digest([7; 32]))).unwrap().is_none());
    assert!(ready(sim.token_info(&source)).unwrap().is_none(), "the mint is not indexed yet");

    sim.lock().mine(0, 0, 0);
    assert_eq!(
        ready(sim.token_info(&source))
            .unwrap()
            .unwrap()
            .emission_amount,
        7
    );

    // Reorged out: unknown again once the indexer recovers.
    sim.lock().orphan(2);
    sim.lock().mine(0, 0, 1);
    sim.lock().settle_indexer();
    assert!(ready(sim.token_info(&source)).unwrap().is_none());
}

#[test]
fn indexed_box_returns_spent_boxes() {
    let mut chain = new_chain();
    let source = box_of(&chain, 0);
    let to = chain.trees()[1].clone();
    let tx = chain.transfer(&[source], &to);
    chain.mine_txs(vec![tx]);
    let start = chain.config().start_height;
    let sim = SimChain::new(chain);

    let spent = ready(sim.indexed_box(&source)).unwrap().unwrap();
    assert_eq!(spent.inclusion_height, start);
    assert!(ready(sim.indexed_box(&Digest([7; 32]))).unwrap().is_none());
}

#[test]
fn boxes_with_pool_reads_the_utxo_set_and_every_pool_output() {
    let mut chain = new_chain();
    let trees = chain.trees().to_vec();
    let unspent = box_of(&chain, 0);
    let pending_input = box_of(&chain, 1);
    let parent = chain.transfer(&[pending_input], &trees[2]);
    let child = chain.transfer(&[parent.outputs[0].id], &trees[3]);
    let spent = box_of(&chain, 4);
    let confirmed = chain.transfer(&[spent], &trees[0]);
    chain.mine_txs(vec![confirmed]);
    chain.submit(parent.clone());
    chain.submit(child.clone());
    let sim = SimChain::new(chain);

    let missing = Digest([7; 32]);
    let ids = [unspent, missing, spent, pending_input, parent.outputs[0].id, child.outputs[0].id];
    let found: Vec<BoxId> = ready(sim.boxes_with_pool(&ids))
        .unwrap()
        .iter()
        .map(|b| b.id)
        .collect();
    // Like the node: boxes a pending tx spends are still returned, pending outputs included.
    assert_eq!(found, vec![unspent, pending_input, parent.outputs[0].id, child.outputs[0].id]);
}

#[test]
fn info_reports_the_best_full_block_and_indexed_blocks_link_back_from_it() {
    let mut chain = new_chain();
    for seed in 0..3 {
        chain.mine(0, 1, seed);
    }
    let orphaned = chain.tip();
    chain.reorg(1, 0, 9);
    let tip = chain.tip();
    let sim = SimChain::new(chain);

    let info = ready(sim.info()).unwrap();
    assert_eq!(info.best_full_header_id, Some(tip.header_id));
    assert_eq!(info.full_height, Some(tip.height));
    assert_ne!(info.best_full_header_id, Some(orphaned.header_id));

    // The block consumer's walk: parent ids from the tip, through indexed blocks, reach the
    // fork point; the orphaned block is not served.
    sim.lock().settle_indexer();
    let mut cursor = tip.header_id;
    let mut heights = Vec::new();
    while let Some(block) = ready(sim.indexed_block(&cursor)).unwrap() {
        heights.push(block.header.height);
        cursor = block.header.parent_id;
    }
    assert_eq!(heights, (1..=tip.height).rev().collect::<Vec<_>>());
    assert!(
        ready(sim.indexed_block(&orphaned.header_id))
            .unwrap()
            .is_none()
    );
}

#[test]
fn unspent_by_token_lists_every_box_holding_it() {
    let sim = SimChain::new(new_chain());
    for (name, _, parts) in GENESIS_TOKENS {
        let boxes = ready(sim.unspent_by_token(&token_id(name), ALL)).unwrap();
        assert_eq!(boxes.len(), parts.len(), "{name}");
    }
    assert!(
        ready(sim.unspent_by_token(&Digest([7; 32]), ALL))
            .unwrap()
            .is_empty()
    );
}
