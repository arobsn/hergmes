use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::types::{HashDigest, HexBytes};

#[derive(Debug, Deserialize, Serialize)]
pub struct Transaction {
    pub id: HashDigest,
    pub inputs: Vec<TransactionInput>,
    pub outputs: Vec<UTxO>,
    #[serde(rename = "inclusionHeight")]
    pub height: u32,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct UnconfirmedTransaction {
    pub id: HashDigest,
    pub inputs: Vec<TransactionInput>,
    pub outputs: Vec<UTxO>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct TransactionInput {
    #[serde(flatten)]
    pub utxo: UTxO,
    #[serde(rename = "spendingProof")]
    pub spending_proof: SpendingProof,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct SpendingProof {
    #[serde(rename = "proofBytes")]
    pub proof_bytes: HexBytes,
    pub extension: HashMap<String, HexBytes>,
}

#[derive(Debug, Deserialize, Serialize)]
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

#[derive(Debug, Deserialize, Serialize)]
pub struct Token {
    #[serde(rename = "tokenId")]
    pub id: HashDigest,
    pub amount: u64,
}

#[derive(Debug, Deserialize, Serialize)]
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

#[derive(Debug, Deserialize, Serialize)]
pub struct BoxesResponse<T> {
    pub items: Vec<T>,
    pub total: u64,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct NodeBox {
    #[serde(flatten)]
    pub utxo: UTxO,
    pub address: Base58String,
    #[serde(rename = "spentTransactionId")]
    pub spent_transaction_id: Option<HashDigest>,
    #[serde(rename = "spendingHeight")]
    pub spending_height: Option<u32>,
    #[serde(rename = "inclusionHeight")]
    pub inclusion_height: u32,
    #[serde(rename = "spendingProof")]
    pub spending_proof: Option<SpendingProof>,
    #[serde(rename = "globalIndex")]
    pub global_index: u64,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Balance {
    pub confirmed: BalancePart,
    pub unconfirmed: BalancePart,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct BalancePart {
    #[serde(rename = "nanoErgs")]
    pub nano_ergs: u64,
    pub tokens: Vec<BalanceToken>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct BalanceToken {
    #[serde(flatten)]
    pub token: Token,
    pub decimals: u32,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(transparent)]
pub struct Base58String(pub String);
