use serde::{Deserialize, Serialize};

use crate::types::{HashDigest, HexBytes};

pub type BoxId = HashDigest;
pub type TxId = HashDigest;
pub type TokenId = HashDigest;
pub type HeaderId = HashDigest;
pub type ErgoTree = HexBytes;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct BlockHeader {
    pub id: HeaderId,
    #[serde(rename = "parentId")]
    pub parent_id: HeaderId,
    pub height: u32,
}

/// A mempool transaction with its input boxes resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnconfirmedTransaction {
    pub id: TxId,
    pub inputs: Vec<UTxO>,
    pub outputs: Vec<UTxO>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct UTxO {
    #[serde(rename = "boxId")]
    pub id: BoxId,

    #[serde(rename = "ergoTree")]
    pub ergo_tree: ErgoTree,

    #[serde(rename = "creationHeight")]
    pub creation_height: u32,

    pub value: u64,

    #[serde(rename = "assets")]
    pub tokens: Vec<Token>,

    #[serde(rename = "additionalRegisters")]
    pub registers: NonMandatoryRegisters,

    pub index: u16,

    #[serde(rename = "transactionId")]
    pub transaction_id: TxId,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct Token {
    #[serde(rename = "tokenId")]
    pub id: TokenId,
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
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct IndexedBox {
    #[serde(flatten)]
    pub utxo: UTxO,

    #[serde(rename = "inclusionHeight")]
    pub inclusion_height: u32,
}

/// A transaction inside an [`IndexedBlock`]. Inputs are full boxes.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct IndexedTransaction {
    pub id: TxId,
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
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct TokenInfo {
    #[serde(rename = "emissionAmount")]
    pub emission_amount: u64,
    pub name: Option<String>,
    pub decimals: Option<u32>,
}

/// Response of `POST /blockchain/balance`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct BalanceResponse {
    pub confirmed: Balance,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Balance {
    #[serde(rename = "nanoErgs")]
    pub nano_ergs: u64,
    pub tokens: Vec<TokenBalance>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct TokenBalance {
    #[serde(rename = "tokenId")]
    pub token_id: TokenId,
    pub amount: u64,
    pub decimals: Option<u32>,
    pub name: Option<String>,
}
