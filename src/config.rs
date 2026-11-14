//! Configuration, read from environment variables (and `.env`).
//!
//! The relay's own settings start with `HERGMES_`, so they can't clash with other variables
//! in a shared environment. `ERGO_NODE_URL` names the node, not the relay, and has no prefix.
//!
//! The box cache budget and the caps on subscriptions and tracked boxes are not here yet:
//! nothing reads them until REST (step 8) and WebSocket (step 9), and their defaults are
//! still open decisions in DESIGN.md.

use std::time::Duration;

use crate::{
    adapters::{follower, mempool, tokens::SUPPLY_LIMIT},
    engine::{self, TrackingKey},
    types::{
        address::{Address, Network},
        ergo::TokenId,
    },
};

/// The variables read, with their defaults.
pub mod vars {
    /// URL of an indexed Ergo node. Required.
    pub const NODE_URL: &str = "ERGO_NODE_URL";
    /// N, in blocks.
    pub const WINDOW_BLOCKS: &str = "HERGMES_WINDOW_BLOCKS";
    /// G, in blocks.
    pub const GRACE_BLOCKS: &str = "HERGMES_GRACE_BLOCKS";
    /// How often `/info` is read, in milliseconds.
    pub const POLL_INTERVAL_MS: &str = "HERGMES_POLL_INTERVAL_MS";
    /// How often the mempool is fetched even if `/info` shows no change, in milliseconds.
    pub const MEMPOOL_REFRESH_MS: &str = "HERGMES_MEMPOOL_REFRESH_MS";
    /// The largest token emission that can be tracked.
    pub const TOKEN_SUPPLY_LIMIT: &str = "HERGMES_TOKEN_SUPPLY_LIMIT";
    /// Addresses tracked for the life of the process, comma-separated.
    pub const TRACK_ADDRESSES: &str = "HERGMES_TRACK_ADDRESSES";
    /// Token ids tracked for the life of the process, comma-separated.
    pub const TRACK_TOKENS: &str = "HERGMES_TRACK_TOKENS";
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub node_url: String,
    pub engine: engine::Config,
    pub follower: follower::Config,
    pub token_supply_limit: u64,
    /// Addresses tracked from the start, checked against the node's network once it is known.
    pub track_addresses: Vec<Address>,
    pub track_tokens: Vec<TokenId>,
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("environment variable `{0}` must be set")]
    Missing(&'static str),
    #[error("environment variable `{var}`: {reason}")]
    Invalid { var: &'static str, reason: String },
}

impl Config {
    pub fn from_env() -> Result<Self, ConfigError> {
        Self::from_vars(|name| std::env::var(name).ok())
    }

    /// Reads the configuration through `var`. An empty value counts as unset.
    pub fn from_vars(var: impl Fn(&str) -> Option<String>) -> Result<Self, ConfigError> {
        let get = |name: &str| {
            var(name)
                .map(|v| v.trim().to_owned())
                .filter(|v| !v.is_empty())
        };
        let engine_defaults = engine::Config::default();
        let follower_defaults = follower::Config::default();

        let window = number(vars::WINDOW_BLOCKS, get(vars::WINDOW_BLOCKS), engine_defaults.window)?;
        if window == 0 {
            return Err(invalid(vars::WINDOW_BLOCKS, "the window keeps at least the tip"));
        }
        let interval = millis(
            vars::POLL_INTERVAL_MS,
            get(vars::POLL_INTERVAL_MS),
            follower_defaults.interval,
        )?;
        let refresh = millis(
            vars::MEMPOOL_REFRESH_MS,
            get(vars::MEMPOOL_REFRESH_MS),
            follower_defaults.mempool.refresh,
        )?;

        Ok(Self {
            node_url: get(vars::NODE_URL).ok_or(ConfigError::Missing(vars::NODE_URL))?,
            engine: engine::Config {
                window,
                grace: number(vars::GRACE_BLOCKS, get(vars::GRACE_BLOCKS), engine_defaults.grace)?,
            },
            follower: follower::Config { interval, mempool: mempool::Config { refresh } },
            token_supply_limit: number(
                vars::TOKEN_SUPPLY_LIMIT,
                get(vars::TOKEN_SUPPLY_LIMIT),
                SUPPLY_LIMIT,
            )?,
            track_addresses: list(vars::TRACK_ADDRESSES, get(vars::TRACK_ADDRESSES), |a| {
                Address::decode(a).map_err(|e| e.to_string())
            })?,
            track_tokens: list(vars::TRACK_TOKENS, get(vars::TRACK_TOKENS), |t| {
                t.parse::<TokenId>().map_err(|e| e.to_string())
            })?,
        })
    }

    /// The keys tracked from the start. Fails if an address is for another network than the
    /// node's: a testnet and a mainnet P2PK address with the same key have the same tree.
    pub fn tracked_keys(&self, network: Network) -> Result<Vec<TrackingKey>, ConfigError> {
        let mut keys = Vec::new();
        for address in &self.track_addresses {
            if address.network() != network {
                return Err(invalid(
                    vars::TRACK_ADDRESSES,
                    &format!(
                        "{address} is a {:?} address, but the node is on {network:?}",
                        address.network()
                    ),
                ));
            }
            keys.push(TrackingKey::Tree(address.ergo_tree().clone()));
        }
        keys.extend(self.track_tokens.iter().map(|id| TrackingKey::Token(*id)));
        keys.sort();
        keys.dedup();
        Ok(keys)
    }
}

fn invalid(var: &'static str, reason: &str) -> ConfigError {
    ConfigError::Invalid { var, reason: reason.to_owned() }
}

fn number<T: std::str::FromStr>(
    var: &'static str,
    value: Option<String>,
    default: T,
) -> Result<T, ConfigError>
where
    T::Err: std::fmt::Display,
{
    match value {
        None => Ok(default),
        Some(v) => v
            .parse()
            .map_err(|e: T::Err| invalid(var, &format!("{v:?}: {e}"))),
    }
}

/// A duration in milliseconds, at least 1: tokio's interval panics on zero.
fn millis(
    var: &'static str,
    value: Option<String>,
    default: Duration,
) -> Result<Duration, ConfigError> {
    let ms = number(var, value, default.as_millis() as u64)?;
    if ms == 0 {
        return Err(invalid(var, "must be at least 1 ms"));
    }
    Ok(Duration::from_millis(ms))
}

fn list<T>(
    var: &'static str,
    value: Option<String>,
    parse: impl Fn(&str) -> Result<T, String>,
) -> Result<Vec<T>, ConfigError> {
    value
        .iter()
        .flat_map(|v| v.split(','))
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(|v| parse(v).map_err(|e| invalid(var, &format!("{v:?}: {e}"))))
        .collect()
}

#[cfg(test)]
#[path = "config_tests.rs"]
mod tests;
