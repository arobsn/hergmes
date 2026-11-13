use std::{
    collections::{BTreeMap, HashMap},
    sync::Arc,
};

use serde::Serialize;

use crate::types::ergo::{BlockHeader, BoxId, HeaderId, TokenId, TxId, UTxO};

/// A block's position and identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BlockRef {
    pub height: u32,
    pub header_id: HeaderId,
}

impl From<&BlockHeader> for BlockRef {
    fn from(header: &BlockHeader) -> Self {
        Self { height: header.height, header_id: header.id }
    }
}

/// A box's position in a key's set, which is also the keyset pagination order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct BoxKey {
    pub inclusion_height: u32,
    pub box_id: BoxId,
}

/// A box leaving a key's set, and the transaction that spent it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Spend {
    pub box_id: BoxId,
    pub tx_id: TxId,
}

/// ERG and token totals over a set of boxes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Totals {
    pub nano_ergs: u64,
    pub tokens: BTreeMap<TokenId, u64>,
}

impl Totals {
    /// Adds a box's ERG and tokens. A token with no units is left out, so two totals over the
    /// same boxes are equal as values.
    pub fn add(&mut self, utxo: &UTxO) {
        self.nano_ergs += utxo.value;
        for token in utxo.tokens.iter().filter(|t| t.amount > 0) {
            *self.tokens.entry(token.id).or_default() += token.amount;
        }
    }

    /// Removes a box added before. A token whose units drop to zero is left out.
    ///
    /// Panics if the box's amounts were never added, which would be a bookkeeping bug.
    pub fn sub(&mut self, utxo: &UTxO) {
        self.nano_ergs = self
            .nano_ergs
            .checked_sub(utxo.value)
            .expect("the box's ERG was added");
        for token in utxo.tokens.iter().filter(|t| t.amount > 0) {
            let amount = self
                .tokens
                .get_mut(&token.id)
                .expect("the box's token was added");
            *amount = amount
                .checked_sub(token.amount)
                .expect("the box's token units were added");
            if *amount == 0 {
                self.tokens.remove(&token.id);
            }
        }
    }
}

/// A small id standing for a tracked key in per-box structures (the reverse index and the
/// ring buffer), so they do not copy an ErgoTree per box. Never reused: an untracked key's
/// deltas are dropped, and a key tracked again gets a new id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct KeyId(pub(super) u32);

impl KeyId {
    pub(super) const MIN: KeyId = KeyId(u32::MIN);
    pub(super) const MAX: KeyId = KeyId(u32::MAX);
}

/// A block of the engine's chain, with its parent: what the block consumer walks back to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChainLink {
    pub block: BlockRef,
    pub parent_id: HeaderId,
}

impl ChainLink {
    /// The block before this one.
    pub fn parent(&self) -> BlockRef {
        BlockRef { height: self.block.height - 1, header_id: self.parent_id }
    }
}

/// A full box kept in the ring buffer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredBox {
    pub inclusion_height: u32,
    pub utxo: Arc<UTxO>,
}

/// One key's share of a block, net of boxes created and spent within the block.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KeyDelta {
    pub created: Vec<BoxId>,
    pub spent: Vec<Spend>,
}

impl KeyDelta {
    pub fn is_empty(&self) -> bool {
        self.created.is_empty() && self.spent.is_empty()
    }
}

/// A ring buffer entry: what one block did to the tracked keys.
///
/// Each box a tracked key gained or lost is stored once, in `boxes`; the per-key
/// deltas refer to it by id. Keys the block left unchanged have no entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockDelta {
    pub block: BlockRef,
    pub parent_id: HeaderId,
    /// The engine's count of applied blocks when this one was applied. A key loaded later
    /// (with a higher count) has no share in it.
    pub seq: u64,
    pub boxes: HashMap<BoxId, StoredBox>,
    pub keys: HashMap<KeyId, KeyDelta>,
}

impl BlockDelta {
    /// The block before this one.
    pub fn parent(&self) -> BlockRef {
        BlockRef { height: self.block.height - 1, header_id: self.parent_id }
    }
}
