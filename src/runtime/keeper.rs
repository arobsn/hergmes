//! The keeper: holds the keys named in the configuration tracked for the life of the
//! process.
//!
//! It loads each key and holds a subscription to it, so the engine never untracks it for
//! idleness. The subscription is balance-only because the keeper reads nothing else: the
//! kind only filters what reaches the keeper's sink, and the engine tracks the key's boxes
//! the same either way, for every other subscriber and query.
//!
//! The engine still drops a key whose load block is orphaned, or every key on a fork deeper
//! than the window; the subscription then ends with `resync_required`, and the keeper loads
//! the key again. A subscription that ends without one (a slow drain) leaves the key
//! tracked, so the keeper subscribes again without a reload.
//!
//! One key's trouble never stops the relay, which serves every other key and client. A token
//! above the supply limit is logged and given up; a node that returns boxes not matching the
//! key is logged and asked again later. Only a stopped engine ends every keeper.

use std::{sync::Arc, time::Duration};

use tokio::{sync::mpsc, time::sleep};
use tracing::{debug, error, info, warn};

use crate::{
    adapters::{
        EngineGone, backoff,
        loader::{LoadError, Loader},
        tokens::{TokenChecker, TokenError},
    },
    clients::NodeSource,
    engine::{Output, TrackingKey},
    runtime::{Handle, Subscription},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Config {
    /// The wait before trying again after an unknown token or a node error the loader does
    /// not wait for itself.
    pub retry: Duration,
    /// The wait after a load returned a box that does not match the key. That is a node
    /// fault; each retry is a full reload, so it is spaced well apart.
    pub mismatch_retry: Duration,
}

impl Default for Config {
    fn default() -> Self {
        Self { retry: Duration::from_secs(5), mismatch_retry: Duration::from_secs(60) }
    }
}

/// Outputs a keeper's sink holds. It takes only `balance` messages and drains them as they
/// come, so a few are plenty.
const SINK: usize = 16;

pub struct Keeper<S> {
    engine: Handle,
    loader: Loader<S>,
    tokens: Arc<TokenChecker<S>>,
    config: Config,
}

impl<S: NodeSource> Keeper<S> {
    pub fn new(
        engine: Handle,
        loader: Loader<S>,
        tokens: Arc<TokenChecker<S>>,
        config: Config,
    ) -> Self {
        Self { engine, loader, tokens, config }
    }

    /// Keeps `key` tracked for good. Returns `Ok` only after giving the key up (a token above
    /// the supply limit, logged at `error`), and `EngineGone` when the engine stops.
    pub async fn keep(&self, key: TrackingKey) -> Result<(), EngineGone> {
        let key = &key;
        // Whether the engine may still track the key. Only a subscription that ended without
        // `resync_required` (a slow drain) leaves it tracked, and a reload would then page
        // through every box for the engine to ignore as a duplicate, so the key is subscribed
        // again first and loaded only if that answers `resync_required`.
        let mut tracked = false;
        loop {
            if !tracked && !self.load(key).await? {
                return Ok(());
            }
            let (sink, mut stream) = mpsc::channel(SINK);
            // A `resync_required`, here or in the stream, is logged by the engine task.
            match self.engine.subscribe(key.clone(), None, sink).await? {
                Subscription::Live { .. } => debug!(?key, "Holding configured key"),
                Subscription::Resync(reason) => {
                    debug!(?key, ?reason, "Configured key not tracked, loading it");
                    tracked = false;
                    continue;
                }
            }
            let mut resync = false;
            while let Some(output) = stream.recv().await {
                resync |= matches!(*output, Output::ResyncRequired { .. });
            }
            tracked = !resync;
            if resync {
                debug!(?key, "Configured key dropped, loading it again");
            } else {
                warn!(?key, "Configured key's subscription ended, subscribing again");
            }
        }
    }

    /// Loads `key`, trying again until the engine tracks it. `false` if the key was given up.
    async fn load(&self, key: &TrackingKey) -> Result<bool, EngineGone> {
        loop {
            if let TrackingKey::Token(id) = key {
                match self.tokens.check(id).await {
                    Ok(()) => {}
                    Err(TokenError::Node(error)) => {
                        let wait = backoff(&error, self.config.retry);
                        warn!(?key, %error, ?wait, "Token check failed, trying again");
                        sleep(wait).await;
                        continue;
                    }
                    Err(TokenError::UnknownToken) => {
                        // The indexer may not have reached the mint yet.
                        warn!(?key, wait = ?self.config.retry, "Configured token is unknown, trying again");
                        sleep(self.config.retry).await;
                        continue;
                    }
                    Err(error @ TokenError::SupplyTooLarge { .. }) => {
                        // Its emission never changes, so asking again can't help.
                        error!(?key, %error, "Configured token can't be tracked, giving it up");
                        return Ok(false);
                    }
                }
            }

            let wait = match self.loader.load(&self.engine, key).await {
                Ok((as_of, boxes)) => {
                    info!(?key, ?as_of, boxes, "Configured key loaded");
                    return Ok(true);
                }
                Err(LoadError::EngineGone(gone)) => return Err(gone),
                Err(LoadError::Mismatch { box_id }) => {
                    let wait = self.config.mismatch_retry;
                    error!(
                        ?key,
                        %box_id,
                        ?wait,
                        "The indexer returned a box that does not match the key, trying again"
                    );
                    wait
                }
                Err(error @ LoadError::GaveUp { wait, .. }) => {
                    // The engine is still catching up with the node, the tip keeps moving,
                    // or the indexer is recovering. The loader skips the wait after its last
                    // attempt, so it is taken here, a `503`'s `Retry-After` included.
                    info!(?key, %error, ?wait, "Configured key not loaded yet, trying again");
                    wait
                }
                Err(LoadError::Node(error)) => {
                    let wait = backoff(&error, self.config.retry);
                    warn!(?key, %error, ?wait, "Load failed, trying again");
                    wait
                }
            };
            sleep(wait).await;
        }
    }
}

#[cfg(test)]
#[path = "keeper_tests.rs"]
mod tests;
