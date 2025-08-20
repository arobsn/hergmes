use crate::models::{HashDigest, ergo_box::ErgoBox};

pub struct ErgoTransaction {
    pub id: HashDigest,
    pub inputs: Vec<ErgoBox>,
    pub outputs: Vec<ErgoBox>,
    pub height: u32,
}
