use serde::{Deserialize, Serialize};

use crate::models::{HashDigest, ergo_box::ErgoBox};

#[derive(Deserialize, Serialize)]
pub struct ErgoTransaction {
    pub id: HashDigest,
    pub inputs: Vec<ErgoBox>,
    pub outputs: Vec<ErgoBox>,
    #[serde(rename = "inclusionHeight")]
    pub height: u32,
}
