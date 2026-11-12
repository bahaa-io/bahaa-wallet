#![allow(dead_code)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use bahaa_config::{Endpoint, Feature, Network, Protocol, Registry};
use bahaa_crypto::{Mnemonic, Seed};
use bahaa_testkit::Reply;
use bahaa_types::{Address, AssetId, ChainFamily};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use serde_json::{Value, json};
use url::Url;

pub type Log = Arc<Mutex<Vec<(String, Value)>>>;

const GENESIS: &str = "5eykt4UsFv8P8NJdTREpY1vzqKqZKvdpKuc147dw2N9d";
const PHRASE: &str =
    "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";
pub const OWNER: &str = "HAgk14JpMQLgt6rVgv7cBQFJWFto5Dqxi472uT3DKpqk";
pub const FRIEND: &str = "2bUBiBNZyD29gP1oV6de7nxowMLoDBtopMMTGgMvjG5m";
pub const USDC: &str = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v";
pub const TOKEN: &str = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA";
const SYSTEM: &str = "11111111111111111111111111111111";
const BLOCKHASH: &str = "Aiyo4Fg2XByNaEhKv3LtX5h9Q8Y2kzhhCZhsnV5fBsrw";
const RENT: u64 = 650_240;
pub const TOKEN_RENT: u64 = 1_488_440;
pub const RECORD_RENT: u64 = 1_056_640;

pub fn solana_on(urls: &[Url]) -> Network {
    let registry = Registry::builtin().unwrap();
    let mut network = registry.get(&"solana".parse().unwrap()).unwrap().clone();
    network.endpoints = urls
        .iter()
        .map(|url| Endpoint {
            protocol: Protocol::SolanaRpc,
            url: url.clone(),
            features: vec![Feature::History, Feature::Nfts],
            min_gap_ms: 0,
            history_gap_ms: 0,
        })
        .collect();
    network
}

pub fn seed() -> Seed {
    Mnemonic::parse(PHRASE).unwrap().to_seed("")
}

pub fn address(text: &str) -> Address {
    Address::parse(ChainFamily::Solana, text).unwrap()
}

fn key(text: &str) -> [u8; 32] {
    let Address::Solana(key) = address(text) else {
        unreachable!()
    };
    key
}

pub fn native() -> AssetId {
    "solana".parse().unwrap()
}

pub fn usdc() -> AssetId {
    format!("solana/{USDC}").parse().unwrap()
}

pub fn wallet(lamports: u64) -> Value {
    json!({"lamports": lamports, "owner": SYSTEM, "executable": false, "data": ["", "base64"], "space": 0})
}

pub fn token_account(owner: &str, mint: &str, units: u64) -> Value {
    let mut data = key(mint).to_vec();
    data.extend_from_slice(&key(owner));
    data.extend_from_slice(&units.to_le_bytes());
    data.resize(165, 0);
    data[108] = 1;
    json!({"lamports": TOKEN_RENT, "owner": TOKEN, "executable": false, "data": [STANDARD.encode(data), "base64"], "space": 165})
}

#[derive(Clone, Default)]
pub struct Ledger {
    pub accounts: HashMap<String, Value>,
    pub fees: Vec<u64>,
    pub units: u64,
    pub simulation_error: Option<Value>,
    pub sent: Option<Value>,
    pub status: Value,
    pub found: Value,
    pub history: Vec<Value>,
    pub transactions: HashMap<String, Value>,
    pub assets: Value,
}

impl Ledger {
    pub fn answer(&self, method: &str, params: &Value) -> Value {
        match method {
            "getGenesisHash" => json!(GENESIS),
            "getMultipleAccounts" => {
                let listed: Vec<Value> = params[0]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|key| {
                        self.accounts
                            .get(key.as_str().unwrap())
                            .cloned()
                            .unwrap_or(Value::Null)
                    })
                    .collect();
                json!({"context": {"slot": 1}, "value": listed})
            }
            "getMinimumBalanceForRentExemption" => match params[0].as_u64().unwrap() {
                0 => json!(RENT),
                165 => json!(TOKEN_RENT),
                80 => json!(RECORD_RENT),
                other => panic!("rent for {other} bytes"),
            },
            "getLatestBlockhash" => {
                json!({"context": {"slot": 1}, "value": {"blockhash": BLOCKHASH, "lastValidBlockHeight": 9}})
            }
            "getRecentPrioritizationFees" => self
                .fees
                .iter()
                .enumerate()
                .map(|(slot, fee)| json!({"slot": slot, "prioritizationFee": fee}))
                .collect(),
            "simulateTransaction" => {
                json!({"context": {"slot": 1}, "value": {"err": self.simulation_error, "logs": [], "unitsConsumed": self.units}})
            }
            "sendTransaction" => self.sent.clone().unwrap_or(Value::Null),
            "getAssetsByOwner" => self.assets.clone(),
            "getSignatureStatuses" => {
                json!({"context": {"slot": 1}, "value": [self.status]})
            }
            "getTransaction" => self
                .transactions
                .get(params[0].as_str().unwrap())
                .cloned()
                .unwrap_or_else(|| self.found.clone()),
            "getSignaturesForAddress" => {
                let before = params[1]["before"].as_str();
                let start = before.map_or(0, |before| {
                    1 + self
                        .history
                        .iter()
                        .position(|entry| entry["signature"] == before)
                        .unwrap()
                });
                let limit = usize::try_from(params[1]["limit"].as_u64().unwrap()).unwrap();
                self.history
                    .iter()
                    .skip(start)
                    .take(limit)
                    .cloned()
                    .collect()
            }
            other => panic!("unexpected {other}"),
        }
    }
}

pub async fn node(log: Log, ledger: Ledger) -> Url {
    serve(log, move |method, params| {
        (
            200,
            json!({"jsonrpc": "2.0", "id": 1, "result": ledger.answer(method, params)}),
        )
    })
    .await
}

pub async fn serve(
    log: Log,
    reply: impl Fn(&str, &Value) -> (u16, Value) + Send + Sync + 'static,
) -> Url {
    bahaa_testkit::serve(move |request| {
        let call = request.json();
        let method = call["method"].as_str().unwrap().to_owned();
        log.lock()
            .unwrap()
            .push((method.clone(), call["params"].clone()));
        let (status, answer) = reply(&method, &call["params"]);
        Reply::json(status, &answer)
    })
    .await
}

pub fn called(log: &Log, method: &str) -> Vec<Value> {
    log.lock()
        .unwrap()
        .iter()
        .filter(|(name, _)| name == method)
        .map(|(_, params)| params.clone())
        .collect()
}
