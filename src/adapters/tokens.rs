//! The token checker: only small-supply tokens can be tracked.
//!
//! Each box holding a token holds at least one unit, so a token with `emissionAmount <= 100`
//! has at most 100 boxes (DESIGN.md, "Tracking keys").

use std::{
    collections::HashMap,
    sync::{Mutex, PoisonError},
};

use crate::{
    clients::{NodeError, NodeSource},
    types::ergo::TokenId,
};

/// The default largest emission accepted.
pub const SUPPLY_LIMIT: u64 = 100;

#[derive(Debug, thiserror::Error)]
pub enum TokenError {
    #[error("token supply {emission} is above the limit {limit}")]
    SupplyTooLarge { emission: u64, limit: u64 },
    /// The indexer does not know the token: it does not exist, or the indexer has not reached
    /// its mint yet.
    #[error("unknown token")]
    UnknownToken,
    /// A `503` is a wait, never an unknown token.
    #[error(transparent)]
    Node(#[from] NodeError),
}

pub struct TokenChecker<S> {
    source: S,
    limit: u64,
    /// Emission amounts read so far. A token's emission never changes, so they are kept for
    /// good. Unknown tokens are not kept: the indexer may reach their mint later.
    emissions: Mutex<HashMap<TokenId, u64>>,
}

impl<S: NodeSource> TokenChecker<S> {
    pub fn new(source: S, limit: u64) -> Self {
        Self { source, limit, emissions: Mutex::new(HashMap::new()) }
    }

    /// Accepts the token if its emission is at most the limit.
    pub async fn check(&self, id: &TokenId) -> Result<(), TokenError> {
        let cached = self
            .emissions
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(id)
            .copied();
        let emission = match cached {
            Some(emission) => emission,
            None => {
                let info = self
                    .source
                    .token_info(id)
                    .await?
                    .ok_or(TokenError::UnknownToken)?;
                self.emissions
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .insert(*id, info.emission_amount);
                info.emission_amount
            }
        };
        if emission > self.limit {
            return Err(TokenError::SupplyTooLarge { emission, limit: self.limit });
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "tokens_tests.rs"]
mod tests;
