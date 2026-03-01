use std::{env, sync::LazyLock};

pub static ERGO_NODE_URL: LazyLock<String> = LazyLock::new(|| get_var("ERGO_NODE_URL"));

fn get_var(key: &str) -> String {
    env::var(key).unwrap_or_else(|_| panic!("Environment variable `{key}` must be set"))
}
