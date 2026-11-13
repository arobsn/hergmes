//! The engine's mempool state: the latest snapshot taken at its tip, its canonical order, and
//! the overlay hash last sent for each tracked key.

use std::{collections::BTreeMap, sync::Arc};

use crate::{
    engine::{
        BlockRef, Output, PendingTx, TrackingKey,
        overlay::{self, OverlayHash, PendingTotals},
    },
    types::ergo::{TxId, UTxO, UnconfirmedTransaction},
};

/// A snapshot the engine accepted, with its canonical order.
#[derive(Debug, Clone)]
struct Snapshot {
    txs: Arc<[UnconfirmedTransaction]>,
    tip: BlockRef,
    order: Vec<usize>,
}

#[derive(Debug, Clone, Default)]
pub struct Mempool {
    snapshot: Option<Snapshot>,
    /// Per tracked key, the hash of the overlay its subscribers have.
    sent: BTreeMap<TrackingKey, OverlayHash>,
}

impl Mempool {
    /// Takes a snapshot observed at `observed_tip` while the engine is at `tip`, and returns a
    /// `Mempool` message for each tracked key whose overlay changed, in key order.
    ///
    /// A snapshot observed at any other block is ignored, with no message and no change: its
    /// confirmed state is not the engine's, so an overlay from it could spend boxes clients do
    /// not have yet, or list txs a block already confirmed.
    ///
    /// `is_confirmed` is asked about the inputs no listed tx creates (see
    /// [`overlay::pending_order`]). The source drops txs whose parent it could not resolve, so
    /// such an input was in the node's UTXO set when it answered; for a box a tracked key holds,
    /// the engine checks its own set instead, since the node's state can run ahead of its pool.
    pub fn apply_snapshot(
        &mut self,
        txs: Arc<[UnconfirmedTransaction]>,
        observed_tip: BlockRef,
        tip: Option<BlockRef>,
        is_confirmed: impl Fn(&UTxO) -> bool,
    ) -> Vec<Output> {
        if tip != Some(observed_tip) {
            return Vec::new();
        }
        let order = overlay::pending_order(&txs, is_confirmed);
        self.snapshot = Some(Snapshot { txs, tip: observed_tip, order });
        self.emit()
    }

    /// The snapshot in use and the tip it was taken at.
    pub fn latest(&self) -> Option<(Arc<[UnconfirmedTransaction]>, BlockRef)> {
        self.snapshot.as_ref().map(|s| (s.txs.clone(), s.tip))
    }

    /// `key`'s overlay, tracked or not. Empty before the first snapshot.
    pub fn overlay(&self, key: &TrackingKey) -> Vec<PendingTx> {
        match &self.snapshot {
            Some(s) => overlay::overlay(&s.txs, &s.order, key),
            None => Vec::new(),
        }
    }

    /// `key`'s pending totals over its overlay, tracked or not.
    pub fn pending_totals(&self, key: &TrackingKey) -> PendingTotals {
        match &self.snapshot {
            Some(s) => overlay::pending_totals(&s.txs, &s.order, key),
            None => PendingTotals::default(),
        }
    }

    /// Starts sending `key`'s overlay when it changes. Its subscribers get the current one
    /// with their catch-up, so it counts as sent.
    pub fn track(&mut self, key: TrackingKey) {
        if !self.sent.contains_key(&key) {
            let hash = overlay::hash(&self.overlay(&key));
            self.sent.insert(key, hash);
        }
    }

    pub fn untrack(&mut self, key: &TrackingKey) {
        self.sent.remove(key);
    }

    pub fn is_tracked(&self, key: &TrackingKey) -> bool {
        self.sent.contains_key(key)
    }

    /// A message for each tracked key whose overlay hash differs from the one sent.
    fn emit(&mut self) -> Vec<Output> {
        let Some(snapshot) = &self.snapshot else {
            return Vec::new();
        };
        let mut overlays = self.tracked_overlays(snapshot);
        let mut outputs = Vec::new();
        for (key, sent) in &mut self.sent {
            let overlay = overlays.remove(key).unwrap_or_default();
            let hash = overlay::hash(&overlay);
            if hash != *sent {
                *sent = hash;
                outputs.push(Output::Mempool {
                    key: key.clone(),
                    tip_height: snapshot.tip.height,
                    pending: overlay,
                });
            }
        }
        outputs
    }

    /// Every tracked key's overlay, in one pass over the snapshot. A key no pending tx touches
    /// is absent.
    fn tracked_overlays(&self, snapshot: &Snapshot) -> BTreeMap<TrackingKey, Vec<PendingTx>> {
        let mut overlays: BTreeMap<TrackingKey, Vec<PendingTx>> = BTreeMap::new();
        for tx in snapshot.order.iter().map(|&i| &snapshot.txs[i]) {
            let mut touched: BTreeMap<TrackingKey, PendingTx> = BTreeMap::new();
            for output in &tx.outputs {
                let mut shared: Option<Arc<UTxO>> = None;
                for key in overlay::keys_of(output).filter(|k| self.sent.contains_key(k)) {
                    let utxo = shared
                        .get_or_insert_with(|| Arc::new(output.clone()))
                        .clone();
                    pending_entry(&mut touched, key, tx.id).created.push(utxo);
                }
            }
            for input in &tx.inputs {
                for key in overlay::keys_of(input).filter(|k| self.sent.contains_key(k)) {
                    pending_entry(&mut touched, key, tx.id).spent.push(input.id);
                }
            }
            for (key, pending) in touched {
                overlays.entry(key).or_default().push(pending);
            }
        }
        overlays
    }
}

fn pending_entry(
    touched: &mut BTreeMap<TrackingKey, PendingTx>,
    key: TrackingKey,
    tx_id: TxId,
) -> &mut PendingTx {
    touched.entry(key).or_insert_with(|| PendingTx {
        tx_id,
        created: Vec::new(),
        spent: Vec::new(),
    })
}

#[cfg(test)]
#[path = "mempool_tests.rs"]
mod tests;
