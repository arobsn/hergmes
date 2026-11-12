//! Deserializes every recorded node response in `tests/fixtures/`.
//!
//! Fixtures are recorded with `scripts/record-fixtures.ts`; `manifest.json` lists the
//! request, status and sample ids behind each one.

use std::path::PathBuf;

use serde::de::DeserializeOwned;
use serde_json::Value;

use super::*;
use crate::types::ergo::{
    BalanceResponse, Block, BlockHeader, IndexedBlock, IndexedBox, IndexedTransaction, TokenInfo,
    UTxO,
};

fn fixture_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(format!("{name}.json"))
}

fn fixture<T: DeserializeOwned>(name: &str) -> T {
    let path = fixture_path(name);
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("parsing {name}.json: {e}"))
}

fn manifest() -> Value {
    fixture("manifest")
}

fn sample(key: &str) -> String {
    manifest()["samples"][key]
        .as_str()
        .unwrap_or_else(|| panic!("sample {key}"))
        .to_string()
}

fn status(name: &str) -> u64 {
    manifest()["fixtures"][name]["status"]
        .as_u64()
        .unwrap_or_else(|| panic!("status {name}"))
}

fn holds(b: &IndexedBox, token_id: &str) -> Option<u64> {
    b.utxo
        .tokens
        .iter()
        .find(|t| t.id.to_string() == token_id)
        .map(|t| t.amount)
}

fn assert_not_found(name: &str) {
    assert_eq!(status(name), 404);
    let err: ApiError = fixture(name);
    assert_eq!(err.error, 404);
    assert_eq!(err.reason, "not-found");
}

#[test]
fn every_manifest_entry_has_a_file() {
    let manifest = manifest();
    let fixtures = manifest["fixtures"].as_object().expect("fixtures map");
    assert!(!fixtures.is_empty());
    for name in fixtures.keys() {
        assert!(fixture_path(name).exists(), "{name}.json missing");
    }
}

#[test]
fn info() {
    let info: InfoResponse = fixture("info");
    assert!(info.last_mempool_update > 0);
}

#[test]
fn indexed_height() {
    let h: IndexedHeightResponse = fixture("indexed_height");
    assert_eq!(h.indexed_height, h.full_height);
}

#[test]
fn last_headers_link_by_parent_id() {
    let mut headers: Vec<BlockHeader> = fixture("last_headers");
    assert_eq!(headers.len(), 3);
    headers.sort_by_key(|h| h.height);
    for pair in headers.windows(2) {
        assert_eq!(pair[1].parent_id, pair[0].id);
        assert_eq!(pair[1].height, pair[0].height + 1);
    }
}

#[test]
fn indexed_block() {
    let block: IndexedBlock = fixture("indexed_block");
    assert_eq!(block.header.id.to_string(), sample("headerId"));
    assert!(!block.transactions.is_empty());

    for (i, tx) in block.transactions.iter().enumerate() {
        assert_eq!(tx.block_id, block.header.id);
        assert_eq!(tx.inclusion_height, block.header.height);
        assert!(!tx.inputs.is_empty());
        for input in &tx.inputs {
            assert_eq!(input.spent_transaction_id, Some(tx.id), "input spent by its tx");
            assert!(input.inclusion_height <= block.header.height);
            // A box can be created and spent in the same block, by an earlier tx.
            if input.inclusion_height == block.header.height {
                let creator = input.utxo.transaction_id;
                assert!(block.transactions[..i].iter().any(|t| t.id == creator));
            }
        }
        for output in &tx.outputs {
            assert_eq!(output.utxo.transaction_id, tx.id);
            assert_eq!(output.inclusion_height, block.header.height);
        }
    }

    // The emission contract NFT moves in every block: spent once, created once.
    let nft = sample("nft");
    let boxes = |side: fn(&IndexedTransaction) -> &Vec<IndexedBox>| {
        block
            .transactions
            .iter()
            .flat_map(side)
            .filter(|b| holds(b, &nft).is_some())
            .count()
    };
    assert_eq!(boxes(|tx| &tx.inputs), 1);
    assert_eq!(boxes(|tx| &tx.outputs), 1);
}

#[test]
fn indexed_block_unknown() {
    assert_not_found("indexed_block_unknown");
}

#[test]
fn plain_block_matches_indexed_block() {
    let plain: Block = fixture("block");
    let indexed: IndexedBlock = fixture("indexed_block");
    assert_eq!(plain.header.id, indexed.header.id);
    let plain_ids: Vec<_> = plain
        .transactions
        .transactions
        .iter()
        .map(|t| t.id)
        .collect();
    let indexed_ids: Vec<_> = indexed.transactions.iter().map(|t| t.id).collect();
    assert_eq!(plain_ids, indexed_ids);
}

#[test]
fn mempool_keeps_resolved_transactions() {
    let txs: Vec<MempoolTransactionResponse> = fixture("mempool");
    let count = txs.len();
    assert_eq!(filter_out_unresolved(txs).len(), count);
}

#[test]
fn mempool_drops_transactions_with_unresolved_inputs() {
    let txs: Vec<MempoolTransactionResponse> = fixture("mempool_unresolved_input");
    assert!(txs[0].inputs[0].utxo.is_none());
    let dropped = txs[0].id;
    let count = txs.len();

    let kept = filter_out_unresolved(txs);
    assert_eq!(kept.len(), count - 1);
    assert!(kept.iter().all(|tx| tx.id != dropped));
}

#[test]
fn unspent_by_address() {
    let boxes: Vec<IndexedBox> = fixture("unspent_by_address");
    assert!(boxes.len() >= 2);
    for b in &boxes {
        assert_eq!(b.utxo.ergo_tree, boxes[0].utxo.ergo_tree, "one address, one tree");
        assert_eq!(b.spent_transaction_id, None);
    }
    // Requested with sortDirection=asc: oldest first.
    assert!(
        boxes
            .windows(2)
            .all(|w| w[0].inclusion_height <= w[1].inclusion_height)
    );
}

#[test]
fn unspent_by_address_empty_is_200_not_404() {
    assert_eq!(status("unspent_by_address_empty"), 200);
    let boxes: Vec<IndexedBox> = fixture("unspent_by_address_empty");
    assert!(boxes.is_empty());
}

#[test]
fn balance() {
    let balance: BalanceResponse = fixture("balance");
    assert!(balance.confirmed.nano_ergs > 0);

    let empty: BalanceResponse = fixture("balance_empty");
    assert_eq!(empty.confirmed.nano_ergs, 0);
    assert!(empty.confirmed.tokens.is_empty());
}

#[test]
fn token_info() {
    let nft: TokenInfo = fixture("token_nft");
    assert_eq!(nft.id.to_string(), sample("nft"));
    assert_eq!(nft.emission_amount, 1);

    let small: TokenInfo = fixture("token_small_supply");
    assert_eq!(small.id.to_string(), sample("smallSupplyToken"));
    assert!((2..=100).contains(&small.emission_amount));

    let large: TokenInfo = fixture("token_large_supply");
    assert!(large.emission_amount > 100);

    assert_not_found("token_unknown");
}

#[test]
fn unspent_by_token_nft_is_one_box() {
    let boxes: Vec<IndexedBox> = fixture("unspent_by_token_nft");
    let nft = sample("nft");
    assert_eq!(boxes.len(), 1);
    assert_eq!(holds(&boxes[0], &nft), Some(1));
}

#[test]
fn unspent_by_token_small_supply_is_bounded_by_emission() {
    let boxes: Vec<IndexedBox> = fixture("unspent_by_token_small_supply");
    let info: TokenInfo = fixture("token_small_supply");
    let token = sample("smallSupplyToken");

    assert!(boxes.len() >= 2);
    assert!(boxes.len() as u64 <= info.emission_amount);
    let total: u64 = boxes
        .iter()
        .map(|b| holds(b, &token).expect("box holds the token"))
        .sum();
    assert!(total <= info.emission_amount);
}

#[test]
fn unspent_by_token_unknown_is_200_not_404() {
    assert_eq!(status("unspent_by_token_unknown"), 200);
    let boxes: Vec<IndexedBox> = fixture("unspent_by_token_unknown");
    assert!(boxes.is_empty());
}

#[test]
fn box_by_id_spent() {
    let b: IndexedBox = fixture("box_by_id_spent");
    assert!(b.spent_transaction_id.is_some());
    let raw: Value = fixture("box_by_id_spent");
    assert!(raw.get("spendingHeight").is_none(), "node now sends spendingHeight");
}

#[test]
fn box_by_id_unknown() {
    assert_not_found("box_by_id_unknown");
}

#[test]
fn with_pool_omits_missing_ids() {
    let requested = manifest()["fixtures"]["with_pool_partial"]["body"].clone();
    let existing = requested[1].as_str().expect("second id exists").to_string();

    let boxes: Vec<UTxO> = fixture("with_pool_partial");
    assert_eq!(boxes.len(), 1);
    assert_eq!(boxes[0].id.to_string(), existing);

    assert_eq!(status("with_pool_none"), 200);
    let none: Vec<UTxO> = fixture("with_pool_none");
    assert!(none.is_empty());
}
