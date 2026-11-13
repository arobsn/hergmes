//! Helpers shared by the simulator's own tests.

use std::{
    future::Future,
    pin::pin,
    task::{Context, Poll, Waker},
};

use proptest::{
    strategy::{Strategy, ValueTree},
    test_runner::TestRunner,
};

use crate::{
    clients::{Page, source::MAX_PAGE_LIMIT},
    sim::{Chain, SimConfig, SimOp, strategy},
    types::{
        Digest, HashDigest, HexBytes,
        ergo::{BoxId, NonMandatoryRegisters, Token, UTxO, UnconfirmedTransaction},
    },
};

/// A page holding every box.
pub const ALL: Page = Page { offset: 0, limit: MAX_PAGE_LIMIT };

pub fn new_chain() -> Chain {
    Chain::new(SimConfig::default())
}

/// The first unspent box, in keyset order, of the chain's `index`-th tree.
pub fn box_of(chain: &Chain, index: usize) -> BoxId {
    let tree = &chain.trees()[index];
    chain
        .utxo()
        .values()
        .find(|b| b.ergo_tree == *tree)
        .expect("the tree owns a box")
        .id
}

/// `len` ops from the real strategy, the same on every run.
pub fn sample_history(len: usize) -> Vec<SimOp> {
    let mut runner = TestRunner::deterministic();
    proptest::collection::vec(strategy::op(4), len)
        .new_tree(&mut runner)
        .expect("strategy")
        .current()
}

/// Runs a future that never waits, as every `SimChain` call is.
pub fn ready<F: Future>(future: F) -> F::Output {
    match pin!(future).poll(&mut Context::from_waker(Waker::noop())) {
        Poll::Ready(output) => output,
        Poll::Pending => panic!("SimChain calls never wait"),
    }
}

pub fn digest(n: u8) -> HashDigest {
    Digest([n; 32])
}

/// A box with id `n` created by tx `tx`, owned by tree `[owner]`, worth 1 nanoERG.
pub fn utxo(n: u8, tx: u8, owner: u8, tokens: &[(u8, u64)]) -> UTxO {
    UTxO {
        id: digest(n),
        ergo_tree: HexBytes(vec![owner]),
        creation_height: 1,
        value: 1,
        tokens: tokens
            .iter()
            .map(|&(id, amount)| Token { id: digest(id), amount })
            .collect(),
        registers: NonMandatoryRegisters::default(),
        index: 0,
        transaction_id: digest(tx),
    }
}

pub fn tx(id: u8, inputs: Vec<UTxO>, outputs: Vec<UTxO>) -> UnconfirmedTransaction {
    UnconfirmedTransaction { id: digest(id), inputs, outputs }
}
