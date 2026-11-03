#![allow(dead_code)]

use bahaa_config::{BlockCheckpoint, ChainKind, Endpoint, Feature, Network, Protocol, Registry};
use bahaa_crypto::{Mnemonic, Seed};
use bahaa_testkit::electrum;
use bahaa_types::{Address, AssetId, ChainFamily};
use url::Url;

pub const GENESIS: &str = "000000000019d6689c085ae165831e934ff763ae46a2a6c172b3f1b60a8ce26f";
pub const PHRASE: &str =
    "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";
pub const FIRST: &str = "bc1qcr8te4kr609gcawutmrza0j4xv80jy8z306fyu";
pub const SECOND: &str = "bc1qnjg0jd8228aq7egyzacy8cys3knf9xvrerkf9g";
pub const FIRST_CHANGE: &str = "bc1q8c6fshw2dlwun7ekn9qwf37cu2rn755upcp6el";
pub const FRIEND: &str = "bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t4";

const SPEC: &str = r##"[{
    "id": "bitcoin",
    "name": "Bitcoin",
    "color": "#f7931a",
    "family": "bitcoin",
    "genesis": "000000000019d6689c085ae165831e934ff763ae46a2a6c172b3f1b60a8ce26f",
    "checkpoint": {"height": 967680, "hash": "0000000000000000000161354016172f2a377610d52dd46d0f68a919333458cd"},
    "pow_limit": "1d00ffff",
    "testnet": false,
    "native": {"symbol": "BTC", "name": "Bitcoin", "decimals": 8},
    "endpoints": [{"protocol": "electrum", "url": "ssl://electrum.blockstream.info:50002", "features": ["history"]}],
    "explorer": {"tx": "https://mempool.space/tx/{tx}", "address": "https://mempool.space/address/{address}"}
}]"##;

pub fn bitcoin_on(url: &Url) -> Network {
    let mut network = bitcoin_with(&[]);
    network.endpoints[0].url = url.clone();
    network
}

pub fn mainnet_on(url: &Url) -> Network {
    let registry = Registry::from_json(SPEC).unwrap();
    let mut network = registry.get(&"bitcoin".parse().unwrap()).unwrap().clone();
    network.endpoints[0].url = url.clone();
    network
}

pub fn bitcoin_with(extra: &[(Protocol, Url)]) -> Network {
    let registry = Registry::from_json(SPEC).unwrap();
    let mut network = registry.get(&"bitcoin".parse().unwrap()).unwrap().clone();
    let ChainKind::Bitcoin { genesis, .. } = network.kind else {
        unreachable!()
    };
    network.kind = ChainKind::Bitcoin {
        genesis,
        checkpoint: BlockCheckpoint {
            height: electrum::CHECKPOINT,
            hash: electrum::checkpoint(),
        },
        pow_limit: electrum::POW_LIMIT,
    };
    network
        .endpoints
        .extend(extra.iter().map(|(protocol, url)| Endpoint {
            protocol: *protocol,
            url: url.clone(),
            features: vec![Feature::History],
            min_gap_ms: 0,
            history_gap_ms: 0,
        }));
    network
}

pub fn seed() -> Seed {
    Mnemonic::parse(PHRASE).unwrap().to_seed("")
}

pub fn address(text: &str) -> Address {
    Address::parse(ChainFamily::Bitcoin, text).unwrap()
}

pub fn native() -> AssetId {
    "bitcoin".parse().unwrap()
}
