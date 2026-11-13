//! Incremental UTXO state at one block, moved along the block tree by applying and
//! undoing blocks. The node keeps one at the selected tip, the indexer one at its own tip.

use std::{
    collections::{BTreeMap, HashMap},
    sync::Arc,
};

use crate::{
    sim::chain::{GENESIS_PARENT, SimBlock},
    types::ergo::{BoxId, HeaderId, IndexedBox, TokenId, UTxO},
};

/// A box as the ledger knows it.
#[derive(Debug, Clone)]
pub struct Entry {
    pub utxo: Arc<UTxO>,
    pub inclusion_height: u32,
    /// Creation order along this chain, like the indexer's `globalIndex`.
    pub global_index: u64,
}

impl Entry {
    pub fn to_indexed(&self) -> IndexedBox {
        IndexedBox { utxo: (*self.utxo).clone(), inclusion_height: self.inclusion_height }
    }
}

#[derive(Debug, Clone)]
pub struct Ledger {
    pub tip: HeaderId,
    pub height: u32,
    pub unspent: BTreeMap<BoxId, Entry>,
    /// Every box created along this chain, spent or not.
    pub all: HashMap<BoxId, Entry>,
    /// Tokens minted along this chain, with their emission amount.
    pub tokens: HashMap<TokenId, u64>,
    next_index: u64,
}

impl Ledger {
    /// The state before the genesis block at `start_height`.
    pub fn new(start_height: u32) -> Self {
        Self {
            tip: GENESIS_PARENT,
            height: start_height - 1,
            unspent: BTreeMap::new(),
            all: HashMap::new(),
            tokens: HashMap::new(),
            next_index: 0,
        }
    }

    pub fn apply(&mut self, block: &SimBlock) {
        assert_eq!(block.header.parent_id, self.tip, "block does not extend the ledger's tip");
        for tx in &block.txs {
            for input in &tx.inputs {
                if self.unspent.remove(&input.id).is_none() {
                    panic!("tx {} spends box {}, which is not unspent", tx.id, input.id);
                }
            }
            self.tokens.extend(tx.minted());
            for output in &tx.outputs {
                let entry = Entry {
                    utxo: output.clone(),
                    inclusion_height: block.header.height,
                    global_index: self.next_index,
                };
                self.next_index += 1;
                self.unspent.insert(output.id, entry.clone());
                self.all.insert(output.id, entry);
            }
        }
        self.tip = block.header.id;
        self.height = block.header.height;
    }

    pub fn undo(&mut self, block: &SimBlock) {
        assert_eq!(block.header.id, self.tip, "only the tip can be undone");
        for tx in block.txs.iter().rev() {
            for output in tx.outputs.iter().rev() {
                self.unspent.remove(&output.id);
                self.all.remove(&output.id);
                self.next_index -= 1;
            }
            for id in tx.minted().keys() {
                self.tokens.remove(id);
            }
            for input in &tx.inputs {
                let entry = self.all[&input.id].clone();
                self.unspent.insert(input.id, entry);
            }
        }
        self.tip = block.header.parent_id;
        self.height -= 1;
    }

    /// Undoes blocks down to the fork point with `target`'s branch, then applies that branch.
    pub fn move_to(&mut self, blocks: &HashMap<HeaderId, Arc<SimBlock>>, target: HeaderId) {
        let mut path = Vec::new();
        let mut cursor = target;
        let mut cursor_height = blocks[&cursor].header.height;
        while cursor_height > self.height {
            path.push(cursor);
            cursor = blocks[&cursor].header.parent_id;
            cursor_height -= 1;
        }
        while self.height > cursor_height {
            self.undo(&blocks[&self.tip]);
        }
        while self.tip != cursor {
            self.undo(&blocks[&self.tip]);
            path.push(cursor);
            cursor = blocks[&cursor].header.parent_id;
        }
        for id in path.iter().rev() {
            self.apply(&blocks[id]);
        }
    }
}
