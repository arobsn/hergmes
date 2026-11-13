//! The simulated node: a block tree with a selected chain, a mempool, and an indexer
//! that can lag behind the tip or sit on an orphaned branch.

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    sync::Arc,
    time::Duration,
};

use blake2::{Blake2b256, Digest as _};
use reqwest::StatusCode;

use crate::{
    clients::{
        NodeError, Page,
        node::{IndexedHeightResponse, InfoResponse},
        source::MAX_PAGE_LIMIT,
    },
    engine::{BlockRef, BoxKey, TrackingKey},
    sim::{
        Endpoint,
        ledger::{Entry, Ledger},
        oracle::Oracle,
        rng::Rng,
        source::Hook,
    },
    types::{
        Digest, HashDigest, HexBytes,
        ergo::{
            Balance, BlockHeader, BoxId, ErgoTree, HeaderId, IndexedBlock, IndexedBox,
            IndexedTransaction, NonMandatoryRegisters, Token, TokenBalance, TokenId, TokenInfo,
            TxId, UTxO, UnconfirmedTransaction,
        },
    },
};

/// The parent id of the genesis block.
pub const GENESIS_PARENT: HeaderId = Digest([0; 32]);

/// The wait a `503` asks for.
pub const RETRY_AFTER: Duration = Duration::from_secs(1);

const NANO_ERGS: u64 = 1_000_000_000;

/// Tokens issued by the genesis block: name, emission amount, and the amounts of the boxes it
/// is split into. Each part goes to a different tree. The emissions sit on both sides of the
/// relay's default supply limit (100).
pub const GENESIS_TOKENS: [(&str, u64, &[u64]); 6] = [
    ("nft-a", 1, &[1]),
    ("nft-b", 1, &[1]),
    ("small-5", 5, &[2, 2, 1]),
    ("small-100", 100, &[40, 30, 20, 10]),
    ("over-101", 101, &[60, 41]),
    ("large", 1_000_000, &[500_000, 300_000, 200_000]),
];

/// Emission amounts of tokens minted by random txs.
const MINT_EMISSIONS: [u64; 5] = [1, 2, 100, 101, 1_000];

/// The id of a genesis token.
pub fn token_id(name: &str) -> TokenId {
    hash(&[b"token", name.as_bytes()])
}

fn hash(parts: &[&[u8]]) -> HashDigest {
    let mut hasher = Blake2b256::new();
    for part in parts {
        hasher.update(part);
    }
    let mut out = [0; 32];
    out.copy_from_slice(&hasher.finalize());
    Digest(out)
}

/// P2PK-shaped trees, and one arbitrary script as the last tree.
fn make_tree(index: usize, script: bool) -> ErgoTree {
    let seed = hash(&[b"tree", &(index as u64).to_be_bytes()]);
    let head: &[u8] = if script { &[0x10, 0x01, 0x04, 0x00] } else { &[0x00, 0x08, 0xcd, 0x02] };
    HexBytes([head, &seed.0].concat())
}

/// A transaction in a block or in the mempool, with full input boxes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SimTx {
    pub id: TxId,
    pub inputs: Vec<Arc<UTxO>>,
    pub outputs: Vec<Arc<UTxO>>,
}

impl SimTx {
    /// The tokens this tx mints, with the units its outputs hold, as the node's indexer counts
    /// them: a token is minted when its id is the first input's box id, which also works for
    /// pending txs. The genesis tx, which has no inputs, mints every token it holds.
    pub fn minted(&self) -> BTreeMap<TokenId, u64> {
        let first = self.inputs.first().map(|b| b.id);
        let mut minted = BTreeMap::new();
        for token in self.outputs.iter().flat_map(|o| &o.tokens) {
            if first.is_none_or(|id| id == token.id) {
                *minted.entry(token.id).or_default() += token.amount;
            }
        }
        minted
    }

    pub fn to_unconfirmed(&self) -> UnconfirmedTransaction {
        UnconfirmedTransaction {
            id: self.id,
            inputs: self.inputs.iter().map(|b| (**b).clone()).collect(),
            outputs: self.outputs.iter().map(|b| (**b).clone()).collect(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SimBlock {
    pub header: BlockHeader,
    pub txs: Vec<Arc<SimTx>>,
}

impl SimBlock {
    pub fn block_ref(&self) -> BlockRef {
        BlockRef::from(&self.header)
    }
}

/// An output to create.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputSpec {
    pub ergo_tree: ErgoTree,
    pub value: u64,
    pub tokens: Vec<Token>,
}

#[derive(Debug, Clone)]
pub struct SimConfig {
    /// Height of the genesis block.
    pub start_height: u32,
    /// How many trees (addresses) own boxes. At least 4, for the genesis token layout.
    pub trees: usize,
    /// How many indexer reads answer `503` after a reorg orphans the indexer's tip.
    pub recovery_reads: u32,
}

impl Default for SimConfig {
    fn default() -> Self {
        Self { start_height: 1, trees: 5, recovery_reads: 2 }
    }
}

/// One step of a random history. Box and tx choices are seeds and indexes resolved against
/// the state at the time, so any sequence of ops is valid and proptest can shrink it freely.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SimOp {
    /// Mine a block with up to `from_pool` mempool txs and `fresh` new ones.
    Mine { from_pool: u8, fresh: u8, seed: u64 },
    /// Orphan the last `depth` blocks and mine a branch of `depth + 1 + extra` blocks.
    Reorg { depth: u8, extra: u8, seed: u64 },
    /// Submit a pending tx, often chained on another pending tx.
    MempoolAdd { seed: u64 },
    /// Submit a pending tx that spends an input of the `pick`-th visible pending tx.
    MempoolConflict { pick: u32, seed: u64 },
    /// Remove the `pick`-th visible pending tx, leaving its children in the pool.
    MempoolDrop { pick: u32 },
    /// Keep the indexer this many blocks behind the tip.
    IndexerLag(u8),
}

/// The simulated node and indexer. Driven directly with `&mut self`; [`SimChain`] shares it
/// with adapters through [`NodeSource`].
///
/// - **Node view** (`info`, `last_headers`, `mempool`, `boxes_with_pool`): the selected tip.
/// - **Indexer view** (every `/blockchain` endpoint): the indexer's own tip, which trails the
///   selected tip by the configured lag. When a reorg orphans it, the next
///   [`SimConfig::recovery_reads`] indexer reads answer `503`, then it rolls back and catches up.
///   The node gates on its state instead (rollback pending or checkpoint off the selected
///   chain); counting reads stands in for time. `indexedHeight` is never gated.
/// - **Mempool**: a pool of txs that may chain. Like the node (`ErgoMemPool`), a double spend
///   replaces the pooled tx, a block removes the txs it confirms and their direct double
///   spends, the rest of what it invalidated goes at the next block's cleanup, and an orphaned
///   block's txs return unchecked. It lists only txs whose inputs resolve from the UTXO set or
///   from a pool tx's outputs, so txs left unresolvable stay hidden.
///
/// [`SimChain`]: crate::sim::SimChain
/// [`NodeSource`]: crate::clients::NodeSource
pub struct Chain {
    config: SimConfig,
    trees: Vec<ErgoTree>,
    blocks: HashMap<HeaderId, Arc<SimBlock>>,
    /// The selected chain's header ids, genesis first.
    main: Vec<HeaderId>,
    /// UTXO state at the selected tip.
    node: Ledger,
    /// UTXO state at the indexer's tip.
    indexer: Ledger,
    indexer_lag: u32,
    /// While the indexer recovers from a reorg: how many more reads answer `503`.
    recovery: Option<u32>,
    pool: Vec<Arc<SimTx>>,
    mempool_clock: u64,
    /// Every box ever built, so tests can name inputs by id.
    boxes: HashMap<BoxId, Arc<UTxO>>,
    /// Every token ever minted, in a block or in the mempool.
    known_tokens: BTreeMap<TokenId, u64>,
    nonce: u64,
    pub(super) calls: Vec<Endpoint>,
    pub(super) hook: Option<Hook>,
}

impl Chain {
    pub fn new(config: SimConfig) -> Self {
        let max_parts = GENESIS_TOKENS.iter().map(|t| t.2.len()).max().unwrap_or(0);
        assert!(config.trees >= max_parts, "the genesis layout needs {max_parts} trees");
        assert!(config.start_height >= 1, "heights start at 1");

        let trees = (0..config.trees)
            .map(|i| make_tree(i, i + 1 == config.trees))
            .collect();
        let mut chain = Self {
            node: Ledger::new(config.start_height),
            indexer: Ledger::new(config.start_height),
            config,
            trees,
            blocks: HashMap::new(),
            main: Vec::new(),
            indexer_lag: 0,
            recovery: None,
            pool: Vec::new(),
            mempool_clock: 1_700_000_000_000,
            boxes: HashMap::new(),
            known_tokens: BTreeMap::new(),
            nonce: 0,
            calls: Vec::new(),
            hook: None,
        };
        let genesis = chain.genesis_tx();
        chain.push_block(vec![genesis]);
        // The indexer starts before genesis, which looks like an orphaned tip.
        chain.settle_indexer();
        chain
    }

    /// A chain with `ops` applied in order.
    pub fn from_ops(config: SimConfig, ops: &[SimOp]) -> Self {
        let mut chain = Self::new(config);
        for op in ops {
            chain.apply(op);
        }
        chain
    }

    /// One box per tree with 1000 ERG, and one per tree holding the genesis tokens'
    /// parts, so several boxes hold more than one token.
    fn genesis_tx(&mut self) -> Arc<SimTx> {
        let mut holdings = vec![BTreeMap::new(); self.trees.len()];
        let mut mints = Vec::new();
        for (k, (name, emission, parts)) in GENESIS_TOKENS.iter().enumerate() {
            let id = token_id(name);
            mints.push((id, *emission));
            for (j, amount) in parts.iter().enumerate() {
                holdings[(k + j) % self.trees.len()].insert(id, *amount);
            }
        }

        let plain = self.trees.iter().map(|tree| OutputSpec {
            ergo_tree: tree.clone(),
            value: 1_000 * NANO_ERGS,
            tokens: Vec::new(),
        });
        let holding = self
            .trees
            .iter()
            .zip(holdings)
            .map(|(tree, tokens)| OutputSpec {
                ergo_tree: tree.clone(),
                value: NANO_ERGS,
                tokens: tokens
                    .into_iter()
                    .map(|(id, amount)| Token { id, amount })
                    .collect(),
            });
        let outputs = plain.chain(holding).collect();
        self.build_tx(Vec::new(), outputs, mints, self.config.start_height)
    }

    pub fn apply(&mut self, op: &SimOp) {
        match *op {
            SimOp::Mine { from_pool, fresh, seed } => {
                self.mine(from_pool.into(), fresh.into(), seed);
            }
            SimOp::Reorg { depth, extra, seed } => {
                self.reorg(depth.into(), extra.into(), seed);
            }
            SimOp::MempoolAdd { seed } => {
                self.mempool_add(seed);
            }
            SimOp::MempoolConflict { pick, seed } => {
                self.mempool_conflict(pick, seed);
            }
            SimOp::MempoolDrop { pick } => {
                self.mempool_drop(pick);
            }
            SimOp::IndexerLag(lag) => self.set_indexer_lag(lag.into()),
        }
    }

    // --- Reading the selected chain ---

    pub fn config(&self) -> &SimConfig {
        &self.config
    }

    pub fn tip(&self) -> BlockRef {
        self.blocks[&self.node.tip].block_ref()
    }

    pub fn height(&self) -> u32 {
        self.node.height
    }

    /// The selected chain's block at `height`.
    pub fn header_at(&self, height: u32) -> Option<BlockRef> {
        let index = height.checked_sub(self.config.start_height)?;
        self.main
            .get(index as usize)
            .map(|id| self.blocks[id].block_ref())
    }

    /// Any block ever mined, orphaned or not.
    pub fn block(&self, id: &HeaderId) -> Option<Arc<SimBlock>> {
        self.blocks.get(id).cloned()
    }

    /// Blocks from genesis to `id`, which may be on an orphaned branch.
    pub fn branch(&self, id: &HeaderId) -> Vec<Arc<SimBlock>> {
        let mut blocks = Vec::new();
        let mut cursor = *id;
        while cursor != GENESIS_PARENT {
            let block = self.blocks[&cursor].clone();
            cursor = block.header.parent_id;
            blocks.push(block);
        }
        blocks.reverse();
        blocks
    }

    /// The oracle over the selected chain.
    pub fn oracle(&self) -> Oracle {
        self.oracle_at(&self.node.tip)
    }

    /// The oracle over the branch ending at `id`.
    pub fn oracle_at(&self, id: &HeaderId) -> Oracle {
        Oracle::new(self.branch(id))
    }

    /// The node's unspent boxes at the selected tip, in keyset order.
    pub fn utxo(&self) -> BTreeMap<BoxKey, Arc<UTxO>> {
        self.node
            .unspent
            .values()
            .map(|e| {
                let key = BoxKey { inclusion_height: e.inclusion_height, box_id: e.utxo.id };
                (key, e.utxo.clone())
            })
            .collect()
    }

    pub fn trees(&self) -> &[ErgoTree] {
        &self.trees
    }

    /// Every token ever minted, in a block or in the mempool, with its emission amount.
    pub fn known_tokens(&self) -> &BTreeMap<TokenId, u64> {
        &self.known_tokens
    }

    /// A key for every tree and every known token.
    pub fn keys(&self) -> Vec<TrackingKey> {
        let trees = self.trees.iter().cloned().map(TrackingKey::Tree);
        trees
            .chain(self.known_tokens.keys().copied().map(TrackingKey::Token))
            .collect()
    }

    /// Any box ever built.
    pub fn find_box(&self, id: &BoxId) -> Option<Arc<UTxO>> {
        self.boxes.get(id).cloned()
    }

    // --- Building txs ---

    /// A tx spending `inputs` (any boxes ever built) into `outputs`.
    ///
    /// Panics unless the outputs hold exactly the inputs' ERG, and no token units beyond the
    /// inputs' (fewer is a burn). Whether the inputs are spendable is checked when it is mined.
    pub fn tx(&mut self, inputs: &[BoxId], outputs: Vec<OutputSpec>) -> Arc<SimTx> {
        let inputs = self.resolve(inputs);
        self.build_tx(inputs, outputs, Vec::new(), self.height())
    }

    /// A tx sending everything in `inputs` to one box guarded by `to`.
    pub fn transfer(&mut self, inputs: &[BoxId], to: &ErgoTree) -> Arc<SimTx> {
        let resolved = self.resolve(inputs);
        let output = merge(&resolved, to);
        self.build_tx(resolved, vec![output], Vec::new(), self.height())
    }

    /// A tx minting `emission` units of a token with the id of `input`, and sending them with
    /// everything in `input` to `to`.
    pub fn mint(&mut self, input: &BoxId, emission: u64, to: &ErgoTree) -> Arc<SimTx> {
        let resolved = self.resolve(&[*input]);
        let mut output = merge(&resolved, to);
        output.tokens.push(Token { id: *input, amount: emission });
        output.tokens.sort_by_key(|t| t.id);
        self.build_tx(resolved, vec![output], vec![(*input, emission)], self.height())
    }

    fn resolve(&self, ids: &[BoxId]) -> Vec<Arc<UTxO>> {
        ids.iter()
            .map(|id| {
                self.find_box(id)
                    .unwrap_or_else(|| panic!("unknown box {id}"))
            })
            .collect()
    }

    fn build_tx(
        &mut self,
        inputs: Vec<Arc<UTxO>>,
        outputs: Vec<OutputSpec>,
        mints: Vec<(TokenId, u64)>,
        creation_height: u32,
    ) -> Arc<SimTx> {
        check_balance(&inputs, &outputs, &mints);
        self.nonce += 1;
        let nonce = self.nonce.to_be_bytes();
        let mut parts: Vec<&[u8]> = vec![b"tx", &nonce];
        parts.extend(inputs.iter().map(|b| &b.id.0[..]));
        let id = hash(&parts);

        let outputs = outputs
            .into_iter()
            .enumerate()
            .map(|(index, spec)| {
                let index = u16::try_from(index).expect("too many outputs");
                Arc::new(UTxO {
                    id: hash(&[b"box", &id.0, &index.to_be_bytes()]),
                    ergo_tree: spec.ergo_tree,
                    creation_height,
                    value: spec.value,
                    tokens: spec.tokens,
                    registers: NonMandatoryRegisters::default(),
                    index,
                    transaction_id: id,
                })
            })
            .collect();

        let tx = Arc::new(SimTx { id, inputs, outputs });
        for output in &tx.outputs {
            self.boxes.insert(output.id, output.clone());
        }
        self.known_tokens.extend(tx.minted());
        tx
    }

    /// A tx spending `forced` and up to a few boxes from `available` (often from `recent`),
    /// splitting ERG and tokens over 1 to 4 outputs with random owners. Sometimes it mints a
    /// token or burns part of one.
    fn random_tx(
        &mut self,
        rng: &mut Rng,
        forced: Option<Arc<UTxO>>,
        available: &BTreeMap<BoxId, Arc<UTxO>>,
        recent: &[BoxId],
        creation_height: u32,
    ) -> Option<Arc<SimTx>> {
        let mut inputs: Vec<Arc<UTxO>> = forced.into_iter().collect();
        let wanted = rng.range(1, 3);
        for _ in 0..8 {
            if inputs.len() >= wanted || available.is_empty() {
                break;
            }
            let id = if !recent.is_empty() && rng.chance(1, 3) {
                recent[rng.below(recent.len())]
            } else {
                *available.keys().nth(rng.below(available.len()))?
            };
            if let Some(utxo) = available.get(&id)
                && !inputs.iter().any(|i| i.id == id)
            {
                inputs.push(utxo.clone());
            }
        }
        let first = inputs.first()?.id;

        let mints = if rng.chance(1, 10) {
            vec![(first, MINT_EMISSIONS[rng.below(MINT_EMISSIONS.len())])]
        } else {
            Vec::new()
        };
        let outputs = self.random_outputs(rng, &inputs, &mints);
        Some(self.build_tx(inputs, outputs, mints, creation_height))
    }

    fn random_outputs(
        &self,
        rng: &mut Rng,
        inputs: &[Arc<UTxO>],
        mints: &[(TokenId, u64)],
    ) -> Vec<OutputSpec> {
        let count = rng.range(1, 4);
        let value = inputs.iter().map(|b| b.value).sum();
        let values = split(rng, value, count, false);
        let mut outputs: Vec<OutputSpec> = values
            .into_iter()
            .map(|value| OutputSpec {
                ergo_tree: self.trees[rng.below(self.trees.len())].clone(),
                value,
                tokens: Vec::new(),
            })
            .collect();

        let mut supply: BTreeMap<TokenId, u64> = BTreeMap::new();
        for token in inputs.iter().flat_map(|b| &b.tokens) {
            *supply.entry(token.id).or_default() += token.amount;
        }
        for (id, emission) in mints {
            *supply.entry(*id).or_default() += emission;
        }
        for (id, mut amount) in supply {
            let minted = mints.iter().any(|(m, _)| *m == id);
            if !minted && rng.chance(1, 20) {
                amount -= 1 + rng.next_u64() % amount;
            }
            for (output, part) in outputs.iter_mut().zip(split(rng, amount, count, true)) {
                if part > 0 {
                    output.tokens.push(Token { id, amount: part });
                }
            }
        }
        outputs
    }

    // --- Mining and reorgs ---

    /// Mines a block on the tip with up to `from_pool` pool txs (in pool order, each valid one
    /// taken with probability 3/4) and `fresh` new txs, which may spend outputs of earlier txs
    /// in the same block or boxes a pending tx also spends.
    pub fn mine(&mut self, from_pool: usize, fresh: usize, seed: u64) -> BlockRef {
        let mut rng = Rng::new(seed);
        let height = self.height() + 1;
        let mut available: BTreeMap<BoxId, Arc<UTxO>> = self
            .node
            .unspent
            .iter()
            .map(|(id, e)| (*id, e.utxo.clone()))
            .collect();

        let mut txs = Vec::new();
        for tx in self.pool.clone() {
            if txs.len() == from_pool {
                break;
            }
            if tx.inputs.iter().all(|i| available.contains_key(&i.id)) && rng.chance(3, 4) {
                spend(&mut available, &tx);
                txs.push(tx);
            }
        }

        let mut recent: Vec<BoxId> = txs
            .iter()
            .flat_map(|tx| tx.outputs.iter().map(|o| o.id))
            .collect();
        for _ in 0..fresh {
            if let Some(tx) = self.random_tx(&mut rng, None, &available, &recent, height) {
                spend(&mut available, &tx);
                recent.extend(tx.outputs.iter().map(|o| o.id));
                txs.push(tx);
            }
        }
        self.push_block(txs)
    }

    /// Mines a block with exactly `txs`. Panics if they spend a box that is not unspent.
    pub fn mine_txs(&mut self, txs: Vec<Arc<SimTx>>) -> BlockRef {
        self.push_block(txs)
    }

    fn push_block(&mut self, txs: Vec<Arc<SimTx>>) -> BlockRef {
        self.nonce += 1;
        let parent_id = self.node.tip;
        let id = hash(&[b"block", &parent_id.0, &self.nonce.to_be_bytes()]);
        let header = BlockHeader { id, parent_id, height: self.node.height + 1 };
        let block = Arc::new(SimBlock { header, txs });

        // The node's cleanup re-validates the pool some time after a block, so the txs the
        // previous block invalidated only go now.
        self.clean_pool();
        self.node.apply(&block);
        self.blocks.insert(id, block.clone());
        self.main.push(id);
        // Like `removeWithDoubleSpends`: the block's txs and the pool txs spending the same
        // inputs. Their descendants stay, unresolvable, until the next cleanup.
        let mined: HashSet<TxId> = block.txs.iter().map(|tx| tx.id).collect();
        let spent: HashSet<BoxId> = block
            .txs
            .iter()
            .flat_map(|tx| tx.inputs.iter().map(|i| i.id))
            .collect();
        self.pool.retain(|tx| {
            !mined.contains(&tx.id) && !tx.inputs.iter().any(|i| spent.contains(&i.id))
        });
        // Every applied block republishes the mempool, changed or not.
        self.mempool_clock += 1;
        self.sync_indexer();
        block.block_ref()
    }

    /// Orphans the last `depth` blocks, never genesis, and returns how many it orphaned. Their
    /// txs go back to the pool unchecked, as the node does, ahead of the txs already there.
    /// A reorg is this followed by
    /// mining a longer branch; in between, the selected chain is shorter than before.
    pub fn orphan(&mut self, depth: u32) -> u32 {
        let depth = depth.min(self.height() - self.config.start_height);
        self.update_pool(|chain| {
            let mut returned = Vec::new();
            for _ in 0..depth {
                let id = chain.main.pop().expect("genesis is never orphaned");
                let block = chain.blocks[&id].clone();
                chain.node.undo(&block);
                returned.splice(0..0, block.txs.iter().cloned());
            }
            returned.append(&mut chain.pool);
            chain.pool = returned;
        });
        self.sync_indexer();
        depth
    }

    /// Orphans the last `depth` blocks and mines a branch of `depth + 1 + extra` blocks. The
    /// branch's first block takes most of the returned txs; the others may be dropped by
    /// conflicting fresh txs. Returns the depth actually orphaned (0 means nothing happened).
    pub fn reorg(&mut self, depth: u32, extra: u32, seed: u64) -> u32 {
        let depth = self.orphan(depth);
        if depth == 0 {
            return 0;
        }
        let mut rng = Rng::new(seed);
        for i in 0..=depth + extra {
            let from_pool = if i == 0 { rng.range(0, 8) } else { rng.range(0, 2) };
            let fresh = rng.range(0, 2);
            let seed = rng.next_u64();
            self.mine(from_pool, fresh, seed);
        }
        depth
    }

    // --- Mempool ---

    /// The pool in insertion order, including txs the node cannot resolve.
    pub fn pool(&self) -> &[Arc<SimTx>] {
        &self.pool
    }

    /// Pool txs whose inputs the node resolves, from the UTXO set or from any pool tx's
    /// outputs, in pool order. The node lists the others with bare input ids (ergo#2248).
    pub fn visible_pool(&self) -> Vec<Arc<SimTx>> {
        let produced = self.pool_outputs();
        self.pool
            .iter()
            .filter(|tx| {
                tx.inputs
                    .iter()
                    .all(|i| self.node.unspent.contains_key(&i.id) || produced.contains(&i.id))
            })
            .cloned()
            .collect()
    }

    /// What `GET /transactions/unconfirmed` returns after the unresolved-input filter, in tx id
    /// order, which has nothing to do with dependencies.
    pub fn mempool(&self) -> Vec<UnconfirmedTransaction> {
        let mut txs: Vec<_> = self
            .visible_pool()
            .iter()
            .map(|tx| tx.to_unconfirmed())
            .collect();
        txs.sort_by_key(|tx| tx.id);
        txs
    }

    /// Adds `tx` to the pool, unless it is already there. Pool txs spending any of its inputs
    /// are replaced, as the node does for a tx with a higher fee (a lower-fee double spend
    /// would be rejected, so it is never submitted). Their descendants stay, unresolvable.
    /// Nothing else is checked: a tx the node cannot resolve stays hidden.
    pub fn submit(&mut self, tx: Arc<SimTx>) {
        self.update_pool(|chain| {
            if chain.pool.iter().any(|p| p.id == tx.id) {
                return;
            }
            let contested: HashSet<BoxId> = tx.inputs.iter().map(|i| i.id).collect();
            chain
                .pool
                .retain(|p| !p.inputs.iter().any(|i| contested.contains(&i.id)));
            chain.pool.push(tx);
        });
    }

    /// Removes a tx from the pool, leaving its children. Returns whether it was there.
    pub fn drop_tx(&mut self, id: &TxId) -> bool {
        let before = self.pool.len();
        self.update_pool(|chain| chain.pool.retain(|tx| tx.id != *id));
        self.pool.len() < before
    }

    /// Submits a random tx that conflicts with no pending tx, often spending a pending output.
    pub fn mempool_add(&mut self, seed: u64) -> Option<Arc<SimTx>> {
        let mut rng = Rng::new(seed);
        let (available, recent) = self.pool_spendable();
        let tx = self.random_tx(&mut rng, None, &available, &recent, self.height())?;
        self.submit(tx.clone());
        Some(tx)
    }

    /// Submits a random tx that spends one input of the `pick`-th visible pending tx, which it
    /// replaces.
    pub fn mempool_conflict(&mut self, pick: u32, seed: u64) -> Option<Arc<SimTx>> {
        let visible = self.visible_pool();
        let target = visible.get(pick as usize % visible.len().max(1))?;
        let mut rng = Rng::new(seed);
        let contested = target.inputs[rng.below(target.inputs.len())].clone();
        let (available, recent) = self.pool_spendable();
        let tx = self.random_tx(&mut rng, Some(contested), &available, &recent, self.height())?;
        self.submit(tx.clone());
        Some(tx)
    }

    /// Drops the `pick`-th visible pending tx.
    pub fn mempool_drop(&mut self, pick: u32) -> Option<TxId> {
        let visible = self.visible_pool();
        let id = visible.get(pick as usize % visible.len().max(1))?.id;
        self.drop_tx(&id);
        Some(id)
    }

    /// The node restarts: the pool starts empty, and `lastMemPoolUpdateTime` is the launch
    /// time, so it moves forward.
    pub fn restart_node(&mut self) {
        self.pool.clear();
        self.mempool_clock += 1;
    }

    /// Boxes a new pending tx can spend without a conflict: unspent or created by a visible
    /// pending tx, and not spent by any pending tx. Also returns the pending-created ones.
    fn pool_spendable(&self) -> (BTreeMap<BoxId, Arc<UTxO>>, Vec<BoxId>) {
        let claimed: HashSet<BoxId> = self
            .pool
            .iter()
            .flat_map(|tx| tx.inputs.iter().map(|i| i.id))
            .collect();
        let mut available: BTreeMap<BoxId, Arc<UTxO>> = self
            .node
            .unspent
            .iter()
            .filter(|(id, _)| !claimed.contains(id))
            .map(|(id, e)| (*id, e.utxo.clone()))
            .collect();
        let mut recent = Vec::new();
        for tx in self.visible_pool() {
            for output in tx.outputs.iter().filter(|o| !claimed.contains(&o.id)) {
                available.insert(output.id, output.clone());
                recent.push(output.id);
            }
        }
        (available, recent)
    }

    fn pool_outputs(&self) -> HashSet<BoxId> {
        self.pool
            .iter()
            .flat_map(|tx| tx.outputs.iter().map(|o| o.id))
            .collect()
    }

    /// Runs `change`, and moves `lastMemPoolUpdateTime` forward if the pool changed.
    fn update_pool(&mut self, change: impl FnOnce(&mut Self)) {
        let before: Vec<TxId> = self.pool.iter().map(|tx| tx.id).collect();
        change(self);
        if self.pool.iter().map(|tx| tx.id).ne(before) {
            self.mempool_clock += 1;
        }
    }

    /// Drops pool txs that can never become valid on this chain: an input is neither unspent
    /// nor created by a pool tx. Repeats until stable, so whole chains go.
    fn clean_pool(&mut self) {
        loop {
            let produced = self.pool_outputs();
            let unspent = &self.node.unspent;
            let before = self.pool.len();
            self.pool.retain(|tx| {
                tx.inputs
                    .iter()
                    .all(|i| unspent.contains_key(&i.id) || produced.contains(&i.id))
            });
            if self.pool.len() == before {
                break;
            }
        }
    }

    // --- Indexer ---

    pub fn indexed_tip(&self) -> BlockRef {
        self.blocks[&self.indexer.tip].block_ref()
    }

    pub fn indexer_lag(&self) -> u32 {
        self.indexer_lag
    }

    /// Keeps the indexer `lag` blocks behind the tip. It never moves back on its own chain,
    /// so a larger lag only takes effect as new blocks arrive.
    pub fn set_indexer_lag(&mut self, lag: u32) {
        self.indexer_lag = lag;
        self.sync_indexer();
    }

    /// Whether indexer reads currently answer `503`.
    pub fn is_recovering(&self) -> bool {
        self.recovery.is_some()
    }

    /// Ends any recovery and brings the indexer to the tip with no lag.
    pub fn settle_indexer(&mut self) {
        self.indexer_lag = 0;
        self.recovery = None;
        self.advance_indexer();
    }

    fn on_main(&self, id: &HeaderId) -> bool {
        self.blocks
            .get(id)
            .is_some_and(|b| self.header_at(b.header.height).map(|r| r.header_id) == Some(*id))
    }

    /// Called after every change to the selected chain.
    fn sync_indexer(&mut self) {
        if self.recovery.is_some() {
            return;
        }
        if !self.on_main(&self.indexer.tip) && self.config.recovery_reads > 0 {
            self.recovery = Some(self.config.recovery_reads);
            return;
        }
        self.advance_indexer();
    }

    fn advance_indexer(&mut self) {
        let lagged = self.height().saturating_sub(self.indexer_lag);
        let floor = if self.on_main(&self.indexer.tip) {
            self.indexer.height
        } else {
            self.config.start_height
        };
        let height = lagged
            .max(floor)
            .clamp(self.config.start_height, self.height());
        let target = self.main[(height - self.config.start_height) as usize];
        self.indexer.move_to(&self.blocks, target);
    }

    /// Run before every indexer read: answers `503` while recovering. The last `503` rolls the
    /// indexer back to the selected chain and catches it up.
    pub(super) fn indexer_gate(&mut self) -> Result<(), NodeError> {
        let Some(left) = self.recovery else {
            return Ok(());
        };
        self.recovery = left.checked_sub(1).filter(|&n| n > 0);
        if self.recovery.is_none() {
            self.advance_indexer();
        }
        Err(NodeError::Unavailable { retry_after: Some(RETRY_AFTER) })
    }

    /// Whether the indexer has applied `block`.
    fn indexed(&self, block: &SimBlock) -> bool {
        let mut cursor = self.indexer.tip;
        while cursor != GENESIS_PARENT {
            let header = &self.blocks[&cursor].header;
            if header.height < block.header.height {
                return false;
            }
            if cursor == block.header.id {
                return true;
            }
            cursor = header.parent_id;
        }
        false
    }

    // --- Endpoint answers. `SimChain` serves these through `NodeSource`. ---

    pub fn info(&self) -> InfoResponse {
        InfoResponse { last_mempool_update: self.mempool_clock, network: "mainnet".to_string() }
    }

    /// The last `count` headers of the selected chain, oldest first.
    pub fn last_headers(&self, count: u32) -> Vec<BlockHeader> {
        let start = self.main.len().saturating_sub(count as usize);
        self.main[start..]
            .iter()
            .map(|id| self.blocks[id].header.clone())
            .collect()
    }

    pub fn indexed_height(&self) -> IndexedHeightResponse {
        IndexedHeightResponse { indexed_height: self.indexer.height, full_height: self.height() }
    }

    /// `None` unless the indexer has applied the block.
    pub fn indexed_block(&self, id: &HeaderId) -> Option<IndexedBlock> {
        let block = self.blocks.get(id)?;
        if !self.indexed(block) {
            return None;
        }
        let entry = |b: &Arc<UTxO>| self.indexer.all[&b.id].to_indexed();
        let transactions = block
            .txs
            .iter()
            .map(|tx| IndexedTransaction {
                id: tx.id,
                inputs: tx.inputs.iter().map(entry).collect(),
                outputs: tx.outputs.iter().map(entry).collect(),
            })
            .collect();
        Some(IndexedBlock { header: block.header.clone(), transactions })
    }

    /// The indexer's unspent boxes matching `predicate`, oldest first by global index, as
    /// one offset page. Rejects a limit above [`MAX_PAGE_LIMIT`] like the node.
    pub fn indexed_unspent(
        &self,
        predicate: impl Fn(&UTxO) -> bool,
        page: Page,
    ) -> Result<Vec<IndexedBox>, NodeError> {
        if page.limit > MAX_PAGE_LIMIT {
            return Err(NodeError::Api {
                status: StatusCode::BAD_REQUEST,
                reason: "bad.request".to_string(),
                detail: Some(format!("limit must be at most {MAX_PAGE_LIMIT}")),
            });
        }
        let mut matching: Vec<&Entry> = self
            .indexer
            .unspent
            .values()
            .filter(|e| predicate(&e.utxo))
            .collect();
        matching.sort_by_key(|e| e.global_index);
        Ok(matching
            .into_iter()
            .skip(page.offset as usize)
            .take(page.limit as usize)
            .map(Entry::to_indexed)
            .collect())
    }

    /// `None` unless the indexer has applied the mint.
    pub fn token_info(&self, id: &TokenId) -> Option<TokenInfo> {
        let emission = *self.indexer.tokens.get(id)?;
        Some(TokenInfo { emission_amount: emission })
    }

    /// Any box the indexer has seen created, spent or not.
    pub fn indexed_box(&self, id: &BoxId) -> Option<IndexedBox> {
        self.indexer.all.get(id).map(Entry::to_indexed)
    }

    /// Totals of the indexer's unspent boxes guarded by `ergo_tree`, tokens in id order.
    pub fn confirmed_balance(&self, ergo_tree: &ErgoTree) -> Balance {
        let mut nano_ergs = 0;
        let mut tokens: BTreeMap<TokenId, u64> = BTreeMap::new();
        for entry in self
            .indexer
            .unspent
            .values()
            .filter(|e| e.utxo.ergo_tree == *ergo_tree)
        {
            nano_ergs += entry.utxo.value;
            for token in &entry.utxo.tokens {
                *tokens.entry(token.id).or_default() += token.amount;
            }
        }
        let tokens = tokens
            .into_iter()
            .map(|(token_id, amount)| TokenBalance { token_id, amount })
            .collect();
        Balance { nano_ergs, tokens }
    }

    /// `POST /utxo/withPool/byIds`: boxes in the UTXO set, or created by any pool tx. Like the
    /// node (`withUnconfirmedTransactions`), a box a pool tx spends is still returned. Missing
    /// ids are left out; order follows `ids`.
    pub fn boxes_with_pool(&self, ids: &[BoxId]) -> Vec<UTxO> {
        let created: HashMap<BoxId, &Arc<UTxO>> = self
            .pool
            .iter()
            .flat_map(|tx| &tx.outputs)
            .map(|o| (o.id, o))
            .collect();
        ids.iter()
            .filter_map(|id| {
                self.node
                    .unspent
                    .get(id)
                    .map(|e| &e.utxo)
                    .or_else(|| created.get(id).copied())
            })
            .map(|b| (**b).clone())
            .collect()
    }
}

/// One output holding everything in `inputs`.
fn merge(inputs: &[Arc<UTxO>], to: &ErgoTree) -> OutputSpec {
    let mut tokens: BTreeMap<TokenId, u64> = BTreeMap::new();
    for token in inputs.iter().flat_map(|b| &b.tokens) {
        *tokens.entry(token.id).or_default() += token.amount;
    }
    OutputSpec {
        ergo_tree: to.clone(),
        value: inputs.iter().map(|b| b.value).sum(),
        tokens: tokens
            .into_iter()
            .map(|(id, amount)| Token { id, amount })
            .collect(),
    }
}

fn spend(available: &mut BTreeMap<BoxId, Arc<UTxO>>, tx: &SimTx) {
    for input in &tx.inputs {
        available.remove(&input.id);
    }
    for output in &tx.outputs {
        available.insert(output.id, output.clone());
    }
}

/// Panics unless `outputs` hold exactly the inputs' ERG and no token units beyond what the
/// inputs and `mints` provide. A tx with no inputs (genesis) creates its ERG.
fn check_balance(inputs: &[Arc<UTxO>], outputs: &[OutputSpec], mints: &[(TokenId, u64)]) {
    if let Some(first) = inputs.first() {
        let value_in: u64 = inputs.iter().map(|b| b.value).sum();
        let value_out: u64 = outputs.iter().map(|o| o.value).sum();
        assert_eq!(value_in, value_out, "ERG is not conserved");
        for (id, _) in mints {
            assert_eq!(*id, first.id, "a minted token's id is the first input's box id");
        }
    }
    let mut supply: BTreeMap<TokenId, u64> = BTreeMap::new();
    for token in inputs.iter().flat_map(|b| &b.tokens) {
        *supply.entry(token.id).or_default() += token.amount;
    }
    for (id, emission) in mints {
        *supply.entry(*id).or_default() += emission;
    }
    for token in outputs.iter().flat_map(|o| &o.tokens) {
        assert!(token.amount > 0, "a box holds at least one unit of each token");
        let left = supply
            .get_mut(&token.id)
            .unwrap_or_else(|| panic!("token {} comes from no input or mint", token.id));
        *left = left
            .checked_sub(token.amount)
            .unwrap_or_else(|| panic!("outputs hold more of {} than provided", token.id));
    }
}

/// Splits `total` into `parts` amounts. With `lumpy`, sometimes gives all of it to one part
/// and spreads small totals unit by unit, so tokens get split and merged.
fn split(rng: &mut Rng, total: u64, parts: usize, lumpy: bool) -> Vec<u64> {
    let mut out = vec![0; parts];
    if lumpy && (parts == 1 || rng.chance(1, 3)) {
        out[rng.below(parts)] = total;
        return out;
    }
    if lumpy && total <= 32 {
        for _ in 0..total {
            out[rng.below(parts)] += 1;
        }
        return out;
    }
    let weights: Vec<u64> = (0..parts).map(|_| rng.range(1, 100) as u64).collect();
    let sum: u64 = weights.iter().sum();
    let mut left = total;
    for (slot, weight) in out.iter_mut().zip(&weights).take(parts - 1) {
        *slot = (u128::from(total) * u128::from(*weight) / u128::from(sum)) as u64;
        left -= *slot;
    }
    out[parts - 1] = left;
    out
}

#[cfg(test)]
#[path = "chain_tests.rs"]
mod tests;
