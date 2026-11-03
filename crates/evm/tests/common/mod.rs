#![allow(dead_code)]

use std::sync::{Arc, Mutex};

use bahaa_config::{Endpoint, Feature, Network, Protocol, Registry};
use bahaa_testkit::Reply;
use bahaa_types::{Address, AssetId, ChainFamily};
use serde_json::{Value, json};
use url::Url;

pub type Log = Arc<Mutex<Vec<String>>>;

pub const OWNER: &str = "0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266";
pub const USDT: &str = "0x55d398326f99059fF775485246999027B3197955";
pub const CUSTOM: &str = "0x0E09FaBB73Bd3Ade0a17ECC321fD13a19e81cE82";

pub fn word(value: u128) -> String {
    format!("0x{value:064x}")
}

pub async fn node(
    log: Log,
    reply: impl Fn(&str, &Value) -> (u16, Value) + Send + Sync + 'static,
) -> Url {
    bahaa_testkit::serve(move |request| {
        let call = request.json();
        let method = call["method"].as_str().unwrap().to_owned();
        log.lock().unwrap().push(method.clone());
        let (status, answer) = reply(&method, &call["params"]);
        Reply::json(status, &answer)
    })
    .await
}

pub fn result(value: &Value) -> (u16, Value) {
    (200, json!({"jsonrpc": "2.0", "id": 1, "result": value}))
}

pub fn bsc_on(urls: &[Url]) -> Network {
    network_on("bsc", urls)
}

pub fn network_on(id: &str, urls: &[Url]) -> Network {
    let registry = Registry::builtin().unwrap();
    let mut network = registry.get(&id.parse().unwrap()).unwrap().clone();
    network.endpoints = urls
        .iter()
        .map(|url| Endpoint {
            protocol: Protocol::JsonRpc,
            url: url.clone(),
            features: vec![Feature::Logs],
            min_gap_ms: 0,
            history_gap_ms: 0,
        })
        .collect();
    network
}

pub fn owner() -> Address {
    Address::parse(ChainFamily::Evm, OWNER).unwrap()
}

pub fn assets(list: &[&str]) -> Vec<AssetId> {
    list.iter().map(|asset| asset.parse().unwrap()).collect()
}
