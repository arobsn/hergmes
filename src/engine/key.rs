use crate::types::ergo::{ErgoTree, TokenId, UTxO};

/// What a client tracks: the boxes guarded by an ErgoTree, or the boxes holding a token.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum TrackingKey {
    Tree(ErgoTree),
    Token(TokenId),
}

impl TrackingKey {
    pub fn matches(&self, utxo: &UTxO) -> bool {
        match self {
            TrackingKey::Tree(tree) => utxo.ergo_tree == *tree,
            TrackingKey::Token(id) => utxo.tokens.iter().any(|t| t.id == *id),
        }
    }
}

#[cfg(test)]
#[path = "key_tests.rs"]
mod tests;
