//! The balance reader: an untracked address's confirmed balance from the indexer, with the
//! height it was read at.
//!
//! The indexer is not pinned to a block, so the height comes from `indexedHeight` read before
//! and after the balance call. The caller combines the balance with the engine's pending
//! delta only when that height is the engine's tip height (DESIGN.md, "Balances"). The node's
//! `unconfirmed` field is never read.

use crate::{
    clients::{NodeError, NodeSource},
    types::ergo::{Balance, ErgoTree},
};

/// Reads before giving up while the indexer keeps moving.
pub const ATTEMPTS: u32 = 3;

/// A confirmed balance as of an indexer height.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BalanceAt {
    pub height: u32,
    pub confirmed: Balance,
}

pub struct BalanceReader<S> {
    source: S,
}

impl<S: NodeSource> BalanceReader<S> {
    pub fn new(source: S) -> Self {
        Self { source }
    }

    /// The confirmed balance of `tree`, read while `indexedHeight` stayed the same. `None` if
    /// it moved during each of [`ATTEMPTS`] reads.
    pub async fn read(&self, tree: &ErgoTree) -> Result<Option<BalanceAt>, NodeError> {
        for _ in 0..ATTEMPTS {
            let before = self.source.indexed_height().await?.indexed_height;
            let confirmed = self.source.confirmed_balance(tree).await?;
            let after = self.source.indexed_height().await?.indexed_height;
            if before == after {
                return Ok(Some(BalanceAt { height: before, confirmed }));
            }
        }
        Ok(None)
    }
}

#[cfg(test)]
#[path = "balance_tests.rs"]
mod tests;
