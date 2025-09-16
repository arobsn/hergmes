use once_cell::sync::Lazy;
use std::env;

pub static ERGO_NODE_URL: Lazy<String> = Lazy::new(|| get_env_or_panic("ERGO_NODE_URL"));

fn get_env_or_panic(key: &str) -> String {
    env::var(key).expect(&format!("{key} must be set"))
}
