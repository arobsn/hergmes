//! The box fetcher: full boxes for the ids the engine keeps, from the cheapest source that
//! has them (DESIGN.md, "Fetching full boxes").

use std::{
    collections::HashMap,
    num::NonZeroUsize,
    sync::{Arc, Mutex, PoisonError},
};

use lru::LruCache;
use tracing::debug;

use crate::{
    adapters::{AdapterError, EngineHandle},
    clients::NodeSource,
    engine::{Query, Reply},
    types::ergo::{BoxId, UTxO},
};

/// Ids per `POST /utxo/withPool/byIds`. The route sets no limit of its own
/// (`UtxoApiRoute.withPoolByIds`); this keeps a request body near 70 KB, far below the HTTP
/// server's default entity limit.
pub const CHUNK: usize = 1000;

pub struct BoxFetcher<S> {
    source: S,
    /// Boxes never change, so the cache needs eviction but never invalidation.
    cache: Mutex<LruCache<BoxId, Arc<UTxO>>>,
    chunk: usize,
}

impl<S: NodeSource> BoxFetcher<S> {
    /// A fetcher whose cache holds up to `capacity` boxes.
    pub fn new(source: S, capacity: NonZeroUsize) -> Self {
        Self::with_chunk(source, capacity, CHUNK)
    }

    pub fn with_chunk(source: S, capacity: NonZeroUsize, chunk: usize) -> Self {
        assert!(chunk >= 1);
        Self { source, cache: Mutex::new(LruCache::new(capacity)), chunk }
    }

    /// The boxes of `ids`, in the same order; `None` for an id no source knows.
    ///
    /// Sources in order: the cache; the engine's ring buffer (boxes spent in the last N
    /// blocks, gone from the node's UTXO set); the node's UTXO set and mempool
    /// (`withPool/byIds`, in chunks, matched by id since it leaves missing ids out); and the
    /// indexer (`box/byId`, which also has spent boxes, for a block the node applied before
    /// the engine). Boxes found are cached.
    pub async fn fetch(
        &self,
        engine: &impl EngineHandle,
        ids: &[BoxId],
    ) -> Result<Vec<Option<Arc<UTxO>>>, AdapterError> {
        let mut found: HashMap<BoxId, Arc<UTxO>> = HashMap::new();
        {
            let mut cache = self.cache.lock().unwrap_or_else(PoisonError::into_inner);
            for id in ids {
                if let Some(b) = cache.get(id) {
                    found.insert(*id, b.clone());
                }
            }
        }
        let cached = found.len();
        let mut missing = missing(ids, &found);

        if !missing.is_empty() {
            match engine.query(Query::Boxes(missing.clone())).await? {
                Reply::Boxes(boxes) => found.extend(boxes.into_iter().map(|b| (b.id, b))),
                other => unreachable!("`Query::Boxes` answered {other:?}"),
            }
            missing = self::missing(&missing, &found);
        }
        let from_ring = found.len() - cached;

        for chunk in missing.chunks(self.chunk) {
            let boxes = self.source.boxes_with_pool(chunk).await?;
            found.extend(boxes.into_iter().map(|b| (b.id, Arc::new(b))));
        }
        missing = self::missing(&missing, &found);
        let from_node = found.len() - cached - from_ring;

        for id in &missing {
            if let Some(b) = self.source.indexed_box(id).await? {
                found.insert(*id, Arc::new(b.utxo));
            }
        }
        let from_indexer = found.len() - cached - from_ring - from_node;
        debug!(ids = ids.len(), cached, from_ring, from_node, from_indexer, "Boxes fetched");

        let mut cache = self.cache.lock().unwrap_or_else(PoisonError::into_inner);
        Ok(ids
            .iter()
            .map(|id| {
                let b = found.get(id).cloned();
                if let Some(b) = &b {
                    cache.put(*id, b.clone());
                }
                b
            })
            .collect())
    }
}

/// The ids not found yet, each once, in their first order.
fn missing(ids: &[BoxId], found: &HashMap<BoxId, Arc<UTxO>>) -> Vec<BoxId> {
    let mut seen = std::collections::HashSet::new();
    ids.iter()
        .filter(|id| !found.contains_key(id) && seen.insert(**id))
        .copied()
        .collect()
}

#[cfg(test)]
#[path = "boxes_tests.rs"]
mod tests;
