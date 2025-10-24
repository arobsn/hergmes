use once_cell::sync::Lazy;
use std::env;

pub static ERGO_NODE_URL: Lazy<String> = Lazy::new(|| get_var("ERGO_NODE_URL"));

fn get_var(key: &str) -> String {
    env::var(key).expect(&format!("Environment variable `{}` must be set", key))
}
