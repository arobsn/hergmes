use std::{collections::HashMap, str};

use serde::{Deserialize, Serialize};

use crate::types::{HashDigest, HexBytes};

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct BlockHeader {
    pub id: HashDigest,
    #[serde(rename = "parentId")]
    pub parent_id: HashDigest,
    pub height: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Block {
    pub header: BlockHeader,
    #[serde(rename = "blockTransactions")]
    pub transactions: BlockTransactions,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct BlockTransactions {
    #[serde(rename = "headerId")]
    pub header_id: HashDigest,
    pub transactions: Vec<BlockTransaction>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct MinimalInput {
    #[serde(rename = "boxId")]
    pub id: HashDigest,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct BlockTransaction {
    pub id: HashDigest,
    pub inputs: Vec<MinimalInput>,
    pub outputs: Vec<UTxO>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct Transaction {
    pub id: HashDigest,
    pub inputs: Vec<TransactionInput>,
    pub outputs: Vec<UTxO>,
    #[serde(rename = "inclusionHeight")]
    pub height: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct UnconfirmedTransaction {
    pub id: HashDigest,
    pub inputs: Vec<TransactionInput>,
    pub outputs: Vec<UTxO>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct TransactionInput {
    #[serde(flatten)]
    pub utxo: UTxO,
    #[serde(rename = "spendingProof")]
    pub spending_proof: SpendingProof,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct SpendingProof {
    #[serde(rename = "proofBytes")]
    pub proof_bytes: HexBytes,
    pub extension: HashMap<String, HexBytes>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct UTxO {
    #[serde(rename = "boxId")]
    pub id: HashDigest,

    #[serde(rename = "ergoTree")]
    pub ergo_tree: HexBytes,

    #[serde(rename = "creationHeight")]
    pub creation_height: u32,

    pub value: u64,

    #[serde(rename = "assets")]
    pub tokens: Vec<Token>,

    #[serde(rename = "additionalRegisters")]
    pub registers: NonMandatoryRegisters,

    pub index: u16,

    #[serde(rename = "transactionId")]
    pub transaction_id: HashDigest,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct Token {
    #[serde(rename = "tokenId")]
    pub id: HashDigest,
    pub amount: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct NonMandatoryRegisters {
    #[serde(rename = "R4")]
    pub r4: Option<HexBytes>,
    #[serde(rename = "R5")]
    pub r5: Option<HexBytes>,
    #[serde(rename = "R6")]
    pub r6: Option<HexBytes>,
    #[serde(rename = "R7")]
    pub r7: Option<HexBytes>,
    #[serde(rename = "R8")]
    pub r8: Option<HexBytes>,
    #[serde(rename = "R9")]
    pub r9: Option<HexBytes>,
}

/// A box as returned by the node's extra indexer (`/blockchain/...` endpoints).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct IndexedBox {
    #[serde(flatten)]
    pub utxo: UTxO,

    #[serde(rename = "inclusionHeight")]
    pub inclusion_height: u32,

    #[serde(rename = "spentTransactionId")]
    pub spent_transaction_id: Option<HashDigest>,
}

/// A transaction inside an [`IndexedBlock`]. Inputs are full boxes.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct IndexedTransaction {
    pub id: HashDigest,

    #[serde(rename = "blockId")]
    pub block_id: HashDigest,

    #[serde(rename = "inclusionHeight")]
    pub inclusion_height: u32,

    pub inputs: Vec<IndexedBox>,
    pub outputs: Vec<IndexedBox>,
}

/// A block from `/blockchain/block/byHeaderId/{id}`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct IndexedBlock {
    pub header: BlockHeader,
    pub transactions: Vec<IndexedTransaction>,
}

/// Token metadata from `/blockchain/token/byId/{tokenId}`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct TokenInfo {
    pub id: HashDigest,

    #[serde(rename = "boxId")]
    pub box_id: HashDigest,

    #[serde(rename = "emissionAmount")]
    pub emission_amount: u64,

    pub name: Option<String>,
    pub description: Option<String>,
    pub decimals: Option<u32>,
}

/// Response of `POST /blockchain/balance`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct BalanceResponse {
    pub confirmed: Balance,
    // `unconfirmed` is unreliable and must not be used
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct Balance {
    #[serde(rename = "nanoErgs")]
    pub nano_ergs: u64,
    pub tokens: Vec<TokenBalance>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct TokenBalance {
    #[serde(rename = "tokenId")]
    pub token_id: HashDigest,
    pub amount: u64,
    pub decimals: Option<u32>,
    pub name: Option<String>,
}
