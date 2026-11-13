use std::{
    collections::{BTreeMap, HashMap},
    sync::Arc,
};

use serde::Serialize;

use crate::{
    engine::TrackingKey,
    types::ergo::{BlockHeader, BoxId, HeaderId, TokenId, TxId, UTxO},
};

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

/// A full box kept in the ring buffer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredBox {
    pub inclusion_height: u32,
    pub utxo: Arc<UTxO>,
}

/// One key's share of a block.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KeyDelta {
    pub created: Vec<BoxId>,
    pub spent: Vec<Spend>,
}

/// A ring buffer entry: what one block did to the tracked keys.
///
/// Each box a tracked key gained or lost is stored once, in `boxes`; the per-key
/// deltas refer to it by id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockDelta {
    pub block: BlockRef,
    pub parent_id: HeaderId,
    pub boxes: HashMap<BoxId, StoredBox>,
    pub keys: HashMap<TrackingKey, KeyDelta>,
}
