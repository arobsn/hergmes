//! Proptest strategies for random histories of [`SimOp`]s.

use proptest::prelude::*;

use crate::sim::SimOp;

/// One op, weighted towards what breaks relays: reorgs, and chained, conflicting and
/// dropped pending txs. Reorgs orphan 1 to `max_depth` blocks.
pub fn op(max_depth: u8) -> impl Strategy<Value = SimOp> {
    assert!(max_depth >= 1, "a reorg orphans at least one block");
    prop_oneof![
        4 => (0..4u8, 0..4u8, any::<u64>())
            .prop_map(|(from_pool, fresh, seed)| SimOp::Mine { from_pool, fresh, seed }),
        1 => (1..=max_depth, 0..3u8, any::<u64>())
            .prop_map(|(depth, extra, seed)| SimOp::Reorg { depth, extra, seed }),
        4 => any::<u64>().prop_map(|seed| SimOp::MempoolAdd { seed }),
        1 => (any::<u32>(), any::<u64>())
            .prop_map(|(pick, seed)| SimOp::MempoolConflict { pick, seed }),
        1 => any::<u32>().prop_map(|pick| SimOp::MempoolDrop { pick }),
        1 => (0..3u8).prop_map(SimOp::IndexerLag),
    ]
}

/// Up to `max_len` ops.
pub fn history(max_len: usize, max_depth: u8) -> impl Strategy<Value = Vec<SimOp>> {
    prop::collection::vec(op(max_depth), 0..=max_len)
}
