use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::models::{HashDigest, HexBytes, ergo_box::ErgoBox};

#[derive(Debug, Deserialize, Serialize)]
pub struct ErgoTransactionInput {
    #[serde(flatten)]
    pub utxo: ErgoBox,
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
pub struct ErgoTransaction {
    pub id: HashDigest,
    pub inputs: Vec<ErgoTransactionInput>,
    pub outputs: Vec<ErgoBox>,
    #[serde(rename = "inclusionHeight")]
    pub height: u32,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct ErgoUnconfirmedTransaction {
    pub id: HashDigest,
    pub inputs: Vec<ErgoTransactionInput>,
    pub outputs: Vec<ErgoBox>,
}
