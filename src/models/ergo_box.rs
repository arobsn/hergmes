use serde::{Deserialize, Serialize};

use crate::models::HashDigest;

#[derive(Deserialize, Serialize)]
pub struct ErgoBox {
    #[serde(rename = "boxId")]
    pub id: HashDigest,

    #[serde(rename = "ergoTree")]
    pub ergo_tree: Vec<u8>,
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

#[derive(Deserialize, Serialize)]
pub struct Token {
    #[serde(rename = "tokenId")]
    pub id: HashDigest,
    pub amount: u64,
}

#[derive(Deserialize, Serialize)]
pub struct NonMandatoryRegisters {
    #[serde(rename = "R4")]
    pub r4: Option<Vec<u8>>,
    #[serde(rename = "R5")]
    pub r5: Option<Vec<u8>>,
    #[serde(rename = "R6")]
    pub r6: Option<Vec<u8>>,
    #[serde(rename = "R7")]
    pub r7: Option<Vec<u8>>,
    #[serde(rename = "R8")]
    pub r8: Option<Vec<u8>>,
    #[serde(rename = "R9")]
    pub r9: Option<Vec<u8>>,
}
