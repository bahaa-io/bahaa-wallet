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

pub fn bitcoin_on(url: &Url) -> Network {
    let mut network = bitcoin_with(&[]);
    network.endpoints[0].url = url.clone();
    network
}

pub fn mainnet_on(url: &Url) -> Network {
    let mut network = Registry::builtin()
        .unwrap()
        .get(&"bitcoin".parse().unwrap())
        .unwrap()
        .clone();
    network.endpoints.truncate(1);
    network.endpoints[0].url = url.clone();
    network
}

pub fn bitcoin_with(extra: &[(Protocol, Url)]) -> Network {
    let mut network = mainnet_on(&Url::parse("tcp://127.0.0.1:9").unwrap());
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
