//! The oracle: answers recomputed from scratch by replaying a branch from genesis. Slow
//! and obviously correct. It shares no state or bookkeeping with [`Chain`]: only the
//! blocks, and [`TrackingKey::matches`] as the key predicate.
//!
//! [`Chain`]: crate::sim::Chain

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    sync::Arc,
};

use crate::{
    engine::{BlockRef, BoxKey, PendingTx, Totals, TrackingKey},
    sim::SimBlock,
    types::ergo::{BoxId, TxId, UTxO, UnconfirmedTransaction},
};

pub struct Oracle {
    blocks: Vec<Arc<SimBlock>>,
}

impl Oracle {
    /// `blocks` run from genesis to a tip, each the child of the one before.
    pub fn new(blocks: Vec<Arc<SimBlock>>) -> Self {
        assert!(!blocks.is_empty(), "a branch has at least the genesis block");
        for pair in blocks.windows(2) {
            assert_eq!(pair[1].header.parent_id, pair[0].header.id, "blocks are not linked");
            assert_eq!(pair[1].header.height, pair[0].header.height + 1, "heights skip");
        }
        Self { blocks }
    }

    pub fn tip(&self) -> BlockRef {
        self.blocks[self.blocks.len() - 1].block_ref()
    }

    /// This branch's block at `height`.
    pub fn block(&self, height: u32) -> Option<BlockRef> {
        let index = height.checked_sub(self.blocks[0].header.height)?;
        self.blocks.get(index as usize).map(|b| b.block_ref())
    }

    /// Every unspent box after the block at `height`, in keyset order.
    pub fn utxo(&self, height: u32) -> BTreeMap<BoxKey, Arc<UTxO>> {
        assert!(self.block(height).is_some(), "height {height} is not on this branch");
        let mut unspent: HashMap<BoxId, (u32, Arc<UTxO>)> = HashMap::new();
        for block in self.blocks.iter().take_while(|b| b.header.height <= height) {
            for tx in &block.txs {
                for input in &tx.inputs {
                    let spent = unspent.remove(&input.id);
                    assert!(spent.is_some(), "box {} is spent but not unspent", input.id);
                }
                for output in &tx.outputs {
                    unspent.insert(output.id, (block.header.height, output.clone()));
                }
            }
        }
        unspent
            .into_iter()
            .map(|(box_id, (inclusion_height, utxo))| (BoxKey { inclusion_height, box_id }, utxo))
            .collect()
    }

    /// The key's unspent boxes after the block at `height`, in keyset order.
    pub fn unspent(&self, key: &TrackingKey, height: u32) -> BTreeMap<BoxKey, Arc<UTxO>> {
        let mut utxo = self.utxo(height);
        utxo.retain(|_, b| key.matches(b));
        utxo
    }

    pub fn totals(&self, key: &TrackingKey, height: u32) -> Totals {
        totals(self.unspent(key, height).values().map(|b| &**b))
    }

    /// The key's reference overlay given `mempool`, against the tip's unspent set.
    pub fn overlay(&self, key: &TrackingKey, mempool: &[UnconfirmedTransaction]) -> Vec<PendingTx> {
        let confirmed: HashSet<BoxId> = self
            .utxo(self.tip().height)
            .values()
            .map(|b| b.id)
            .collect();
        overlay(&confirmed, mempool, key)
    }
}

/// ERG and token totals over `boxes`. A token appears only with a non-zero amount.
pub fn totals<'a>(boxes: impl IntoIterator<Item = &'a UTxO>) -> Totals {
    let mut totals = Totals::default();
    for b in boxes {
        totals.nano_ergs += b.value;
        for token in b.tokens.iter().filter(|t| t.amount > 0) {
            *totals.tokens.entry(token.id).or_default() += token.amount;
        }
    }
    totals
}

/// The mempool txs that can be valid on top of `confirmed` (the unspent box ids), in
/// canonical order.
///
/// A tx is included when each input is in `confirmed` or created by an included tx, so a
/// tx whose parent is missing is left out with all its descendants. The node never lists two
/// txs spending the same box; if a snapshot ever did, both would be included.
/// The order is topological, choosing the smallest tx id among the ready txs at each step
/// (Kahn's algorithm), so it depends only on the set of txs.
pub fn pending_order<'a>(
    confirmed: &HashSet<BoxId>,
    mempool: &'a [UnconfirmedTransaction],
) -> Vec<&'a UnconfirmedTransaction> {
    let mut available = confirmed.clone();
    let mut waiting: BTreeMap<TxId, &UnconfirmedTransaction> =
        mempool.iter().map(|tx| (tx.id, tx)).collect();
    let mut order = Vec::new();
    while let Some(id) = waiting
        .values()
        .find(|tx| tx.inputs.iter().all(|i| available.contains(&i.id)))
        .map(|tx| tx.id)
    {
        let tx = waiting.remove(&id).expect("found above");
        available.extend(tx.outputs.iter().map(|o| o.id));
        order.push(tx);
    }
    order
}

/// One key's reference overlay: the txs of [`pending_order`] that create or spend a box
/// matching the key, in that order, each with only its matching boxes.
pub fn overlay(
    confirmed: &HashSet<BoxId>,
    mempool: &[UnconfirmedTransaction],
    key: &TrackingKey,
) -> Vec<PendingTx> {
    filter_overlay(&pending_order(confirmed, mempool), key)
}

/// [`overlay`] over an order already computed by [`pending_order`], to share it between keys.
pub fn filter_overlay(order: &[&UnconfirmedTransaction], key: &TrackingKey) -> Vec<PendingTx> {
    order
        .iter()
        .filter_map(|tx| {
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
            (!created.is_empty() || !spent.is_empty()).then_some(PendingTx {
                tx_id: tx.id,
                created,
                spent,
            })
        })
        .collect()
}

#[cfg(test)]
#[path = "oracle_tests.rs"]
mod tests;
