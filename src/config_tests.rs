use std::collections::HashMap;

use super::*;

const MAINNET: &str = "9fRusAarL1KkrWQVsxSRVYnvWxaAT2A96cKtNn9tvPh5XUyCisr";
const MAINNET_TREE: &str =
    "0008cd0278011ec0cf5feb92d61adb51dcb75876627ace6fd9446ab4cabc5313ab7b39a7";
const FEE_TESTNET: &str = "Bf1X9JgQTUtgntaer91B24n6kP8L2kqEiQqNf1z97BKo9UbnW3WRP9VXu8BXd1LsYCiYbHJEdWKxkF5YNx5n7m31wsDjbEuB3B13ZMDVBWkepGmWfGa71otpFViHDCuvbw1uNicAQnfuWfnj8fbCa4";
const NFT: &str = "20fa2bf23962cdf51b07722d6237c0c7b8a44f78856c0f7ec308dc1ef1a92a51";
const TOKEN: &str = "8c27dd9d8a35aac1e3167d58858c0a8b4059b277da790552e37eba22df9b9035";

fn config(vars: &[(&str, &str)]) -> Result<Config, ConfigError> {
    let vars: HashMap<String, String> = vars
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    Config::from_vars(|name| vars.get(name).cloned())
}

fn invalid_var(result: Result<Config, ConfigError>) -> &'static str {
    match result {
        Err(ConfigError::Invalid { var, .. }) => var,
        other => panic!("expected an invalid variable, got {other:?}"),
    }
}

#[test]
fn only_the_node_url_is_required() {
    let config = config(&[(vars::NODE_URL, "http://127.0.0.1:9053")]).unwrap();
    assert_eq!(config.node_url, "http://127.0.0.1:9053");
    assert_eq!(config.engine, engine::Config::default());
    assert_eq!(config.follower, follower::Config::default());
    assert_eq!(config.token_supply_limit, SUPPLY_LIMIT);
    assert!(config.track_addresses.is_empty() && config.track_tokens.is_empty());

    assert!(matches!(self::config(&[]), Err(ConfigError::Missing(vars::NODE_URL))));
    let empty = self::config(&[(vars::NODE_URL, "  ")]);
    assert!(matches!(empty, Err(ConfigError::Missing(vars::NODE_URL))));
}

#[test]
fn every_setting_is_read() {
    let config = config(&[
        (vars::NODE_URL, "http://node"),
        (vars::WINDOW_BLOCKS, "30"),
        (vars::GRACE_BLOCKS, "0"),
        (vars::POLL_INTERVAL_MS, "250"),
        (vars::MEMPOOL_REFRESH_MS, "5000"),
        (vars::TOKEN_SUPPLY_LIMIT, "7"),
        (vars::TRACK_ADDRESSES, &format!(" {MAINNET} ,")),
        (vars::TRACK_TOKENS, &format!("{NFT},{TOKEN}")),
    ])
    .unwrap();
    assert_eq!(config.engine, engine::Config { window: 30, grace: 0 });
    assert_eq!(config.follower.interval, Duration::from_millis(250));
    assert_eq!(config.follower.mempool.refresh, Duration::from_secs(5));
    assert_eq!(config.token_supply_limit, 7);
    assert_eq!(config.track_addresses, vec![Address::decode(MAINNET).unwrap()]);
    assert_eq!(config.track_tokens, vec![NFT.parse().unwrap(), TOKEN.parse().unwrap()]);
}

#[test]
fn bad_values_name_their_variable() {
    let url = (vars::NODE_URL, "http://node");
    for (var, value) in [
        (vars::WINDOW_BLOCKS, "0"),
        (vars::WINDOW_BLOCKS, "-1"),
        (vars::GRACE_BLOCKS, "five"),
        (vars::POLL_INTERVAL_MS, "0"),
        (vars::MEMPOOL_REFRESH_MS, "1.5"),
        (vars::TOKEN_SUPPLY_LIMIT, "lots"),
        (vars::TRACK_ADDRESSES, "9fRusAarL1KkrWQVsxSRVYnvWxaAT2A96cKtNn9tvPh5XUyCisR"),
        (vars::TRACK_TOKENS, "abcd"),
    ] {
        assert_eq!(invalid_var(config(&[url, (var, value)])), var, "{var}={value}");
    }
}

#[test]
fn tracked_keys_are_trees_and_tokens_on_the_node_network() {
    let config = config(&[
        (vars::NODE_URL, "http://node"),
        (vars::TRACK_ADDRESSES, &format!("{MAINNET},{MAINNET}")),
        (vars::TRACK_TOKENS, NFT),
    ])
    .unwrap();
    let keys = config.tracked_keys(Network::Mainnet).unwrap();
    assert_eq!(
        keys,
        vec![
            TrackingKey::Tree(crate::types::HexBytes(hex::decode(MAINNET_TREE).unwrap())),
            TrackingKey::Token(NFT.parse().unwrap()),
        ],
        "each key once"
    );
    let wrong = config.tracked_keys(Network::Testnet);
    assert!(matches!(wrong, Err(ConfigError::Invalid { var: vars::TRACK_ADDRESSES, .. })));

    let testnet =
        self::config(&[(vars::NODE_URL, "http://node"), (vars::TRACK_ADDRESSES, FEE_TESTNET)])
            .unwrap();
    assert!(testnet.tracked_keys(Network::Mainnet).is_err());
    assert_eq!(testnet.tracked_keys(Network::Testnet).unwrap().len(), 1);
}
