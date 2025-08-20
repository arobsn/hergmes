use crate::models::HashDigest;

pub struct ErgoBox {
    pub id: HashDigest,

    pub ergo_tree: Vec<u8>,
    pub creation_height: u32,
    pub value: u64,
    pub assets: Vec<Token>,
    pub registers: NonMandatoryRegisters,

    pub index: u16,
    pub transaction_id: HashDigest,
}

pub struct Token {
    pub id: HashDigest,
    pub amount: u64,
}

pub struct NonMandatoryRegisters {
    pub r4: Option<Vec<u8>>,
    pub r5: Option<Vec<u8>>,
    pub r6: Option<Vec<u8>>,
    pub r7: Option<Vec<u8>>,
    pub r8: Option<Vec<u8>>,
    pub r9: Option<Vec<u8>>,
}
