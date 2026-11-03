#![allow(dead_code)]

use std::sync::{Arc, Mutex};

use bahaa_config::{Endpoint, Network, Protocol, Registry};
use bahaa_testkit::Reply;
use bahaa_types::{Address, AssetId, ChainFamily};
use serde_json::Value;
use url::Url;

pub type Log = Arc<Mutex<Vec<String>>>;

pub const GENESIS: &str = "00000000000000001ebf88508a03865c71d452e25f4d51194196a1d22b6653dc";
pub const NILE: &str = "0000000000000000d698d4192c56cb6be724a558448e2684802de4d6cd8690dc";
pub const OWNER: &str = "TWer2Ygk5TEheHp3TPuYeqxmB6SsGZmaL6";
pub const OWNER_HEX: &str = "41e2e1a54926527fbb4e4420de4c6bab82beaee24d";
pub const USDT: &str = "TR7NHqjeKQxGTCi8q8ZY4pL8otSzgjLj6t";
pub const USDT_HEX: &str = "41a614f803b6fd780986a42c78ec9c7f77e6ded13c";

pub async fn node(
    log: Log,
    reply: impl Fn(&str, &Value) -> (u16, Value) + Send + Sync + 'static,
) -> Url {
    bahaa_testkit::serve(move |request| {
        let path = request.path().to_owned();
        log.lock().unwrap().push(path.clone());
        let (status, answer) = reply(&path, &request.json());
        Reply::json(status, &answer)
    })
    .await
}

pub fn tron_on(urls: &[Url]) -> Network {
    let registry = Registry::builtin().unwrap();
    let mut network = registry.get(&"tron".parse().unwrap()).unwrap().clone();
    network.endpoints = urls
        .iter()
        .map(|url| Endpoint {
            protocol: Protocol::TronHttp,
            url: url.clone(),
            features: Vec::new(),
            min_gap_ms: 0,
            history_gap_ms: 0,
        })
        .collect();
    network
}

pub fn tron(text: &str) -> Address {
    Address::parse(ChainFamily::Tron, text).unwrap()
}

pub fn assets(list: &[&str]) -> Vec<AssetId> {
    list.iter().map(|asset| asset.parse().unwrap()).collect()
}

pub fn count(log: &Log, path: &str) -> usize {
    log.lock()
        .unwrap()
        .iter()
        .filter(|seen| *seen == path)
        .count()
}
