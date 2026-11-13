//! Recorded node responses in `tests/fixtures/`, for unit tests.
//!
//! Recorded with `scripts/record-fixtures.ts`; `manifest.json` lists the request,
//! status and sample ids behind each one.

use std::path::PathBuf;

use serde::de::DeserializeOwned;
use serde_json::Value;

pub fn path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(format!("{name}.json"))
}

pub fn text(name: &str) -> String {
    let path = path(name);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()))
}

pub fn load<T: DeserializeOwned>(name: &str) -> T {
    serde_json::from_str(&text(name)).unwrap_or_else(|e| panic!("parsing {name}.json: {e}"))
}

pub fn manifest() -> Value {
    load("manifest")
}

pub fn sample(key: &str) -> String {
    manifest()["samples"][key]
        .as_str()
        .unwrap_or_else(|| panic!("sample {key}"))
        .to_string()
}

pub fn status(name: &str) -> u16 {
    manifest()["fixtures"][name]["status"]
        .as_u64()
        .unwrap_or_else(|| panic!("status {name}")) as u16
}
