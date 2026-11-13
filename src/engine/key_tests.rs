use super::*;
use crate::{
    fixtures::{load, sample},
    types::{
        address::Address,
        ergo::{IndexedBlock, IndexedBox},
    },
};

fn token(key: &str) -> TrackingKey {
    TrackingKey::Token(sample(key).parse().unwrap())
}

#[test]
fn address_key_matches_its_unspent_boxes() {
    let key = TrackingKey::Tree(
        Address::decode(&sample("address"))
            .unwrap()
            .into_ergo_tree(),
    );
    let boxes: Vec<IndexedBox> = load("unspent_by_ergo_tree");
    assert!(!boxes.is_empty());
    assert!(boxes.iter().all(|b| key.matches(&b.utxo)));

    let others: Vec<IndexedBox> = load("unspent_by_token_small_supply");
    assert!(others.iter().all(|b| !key.matches(&b.utxo)));
}

#[test]
fn token_key_matches_boxes_holding_the_token() {
    let small = token("smallSupplyToken");
    let boxes: Vec<IndexedBox> = load("unspent_by_token_small_supply");
    assert!(boxes.iter().all(|b| small.matches(&b.utxo)));

    let nft = token("nft");
    assert!(boxes.iter().all(|b| !nft.matches(&b.utxo)));
    let nft_boxes: Vec<IndexedBox> = load("unspent_by_token_nft");
    assert!(nft_boxes.iter().all(|b| nft.matches(&b.utxo)));
}

#[test]
fn a_box_matches_its_tree_and_each_token_it_holds() {
    let boxes: Vec<IndexedBox> = load("unspent_by_token_nft");
    let utxo = &boxes[0].utxo;
    assert!(TrackingKey::Tree(utxo.ergo_tree.clone()).matches(utxo));
    assert!(token("nft").matches(utxo));
    for t in &utxo.tokens {
        assert!(TrackingKey::Token(t.id).matches(utxo));
    }
}

#[test]
fn nft_moves_once_per_block() {
    let block: IndexedBlock = load("indexed_block");
    let nft = token("nft");
    let count =
        |boxes: Vec<&IndexedBox>| boxes.into_iter().filter(|b| nft.matches(&b.utxo)).count();
    assert_eq!(
        count(
            block
                .transactions
                .iter()
                .flat_map(|tx| &tx.inputs)
                .collect()
        ),
        1
    );
    assert_eq!(
        count(
            block
                .transactions
                .iter()
                .flat_map(|tx| &tx.outputs)
                .collect()
        ),
        1
    );
}
