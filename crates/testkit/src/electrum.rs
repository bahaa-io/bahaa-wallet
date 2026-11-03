use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use bahaa_types::{Address, BitcoinScript, hex};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use url::Url;

pub type Log = Arc<Mutex<Vec<(String, Value)>>>;

fn locking(address: &Address) -> Vec<u8> {
    let Address::Bitcoin {
        script: BitcoinScript::WitnessKeyHash(hash),
        ..
    } = address
    else {
        panic!("the fake Electrum server only holds native SegWit addresses")
    };
    [&[0x00, 0x14][..], hash].concat()
}

pub fn scripthash(address: &Address) -> String {
    let mut hash: [u8; 32] = Sha256::digest(locking(address)).into();
    hash.reverse();
    hex::encode(&hash)
}

pub fn txid(raw: &[u8]) -> String {
    let mut hash: [u8; 32] = Sha256::digest(Sha256::digest(raw)).into();
    hash.reverse();
    hex::encode(&hash)
}

pub fn sender(salt: u8) -> Address {
    Address::Bitcoin {
        script: BitcoinScript::WitnessKeyHash([salt; 20]),
        testnet: false,
    }
}

pub fn time_of(height: u64) -> u64 {
    1_200_000_000 + height * 600
}

fn transaction(previous: &[u8; 32], value: u64, to: &Address) -> (String, String) {
    let mut raw = vec![2, 0, 0, 0, 1];
    raw.extend_from_slice(previous);
    raw.extend_from_slice(&[0, 0, 0, 0, 0, 0xff, 0xff, 0xff, 0xff, 1]);
    raw.extend_from_slice(&value.to_le_bytes());
    raw.push(22);
    raw.extend_from_slice(&locking(to));
    raw.extend_from_slice(&[0, 0, 0, 0]);
    (txid(&raw), hex::encode(&raw))
}

fn header(height: u64) -> String {
    let mut raw = vec![0u8; 80];
    let time = u32::try_from(time_of(height)).unwrap();
    raw[68..72].copy_from_slice(&time.to_le_bytes());
    hex::encode(&raw)
}

#[derive(Clone, Debug, Default)]
pub struct Ledger {
    pub genesis: String,
    pub histories: HashMap<String, Vec<Value>>,
    pub unspent: HashMap<String, Vec<Value>>,
    pub transactions: HashMap<String, String>,
    pub broadcast: Vec<String>,
    pub refusal: Option<String>,
}

impl Ledger {
    pub fn new(genesis: &str) -> Self {
        Self {
            genesis: genesis.to_owned(),
            ..Self::default()
        }
    }

    pub fn fund(&mut self, to: &Address, value: u64, salt: u8) -> String {
        self.fund_at(to, value, salt, 900_000)
    }

    pub fn fund_at(&mut self, to: &Address, value: u64, salt: u8, height: u64) -> String {
        let (source, source_raw) = transaction(&[salt; 32], value + 1_000, &sender(salt));
        let mut previous: [u8; 32] = hex::decode_array(&source).unwrap();
        previous.reverse();
        let (id, raw) = transaction(&previous, value, to);
        self.transactions.insert(source, source_raw);
        self.transactions.insert(id.clone(), raw);
        let key = scripthash(to);
        self.histories
            .entry(key.clone())
            .or_default()
            .push(json!({"tx_hash": id, "height": height}));
        self.unspent
            .entry(key)
            .or_default()
            .push(json!({"tx_hash": id, "tx_pos": 0, "height": height, "value": value}));
        id
    }

    pub fn confirm(&mut self, id: &str, address: &Address, height: u64) {
        self.histories
            .entry(scripthash(address))
            .or_default()
            .push(json!({"tx_hash": id, "height": height}));
    }

    pub fn answer(&mut self, method: &str, params: &Value) -> Result<Value, String> {
        let key = params[0].as_str().unwrap_or_default().to_owned();
        Ok(match method {
            "server.version" => json!(["fake", "1.4"]),
            "server.features" => json!({"genesis_hash": self.genesis}),
            "blockchain.scripthash.get_history" => {
                json!(self.histories.get(&key).cloned().unwrap_or_default())
            }
            "blockchain.scripthash.listunspent" => {
                json!(self.unspent.get(&key).cloned().unwrap_or_default())
            }
            "blockchain.block.header" => json!(header(params[0].as_u64().unwrap())),
            "blockchain.estimatefee" => serde_json::from_str("2e-05").unwrap(),
            "blockchain.relayfee" => serde_json::from_str("1e-06").unwrap(),
            "blockchain.transaction.broadcast" => {
                if let Some(refusal) = &self.refusal {
                    return Err(refusal.clone());
                }
                let id = witnessless_id(&hex::decode(&key).unwrap());
                self.broadcast.push(key.clone());
                self.transactions.insert(id.clone(), key);
                json!(id)
            }
            "blockchain.transaction.get" => match self.transactions.get(&key) {
                Some(raw) => json!(raw),
                None => return Err("missing transaction".to_owned()),
            },
            other => return Err(format!("unknown method {other}")),
        })
    }
}

fn witnessless_id(raw: &[u8]) -> String {
    if raw[4] != 0 {
        return txid(raw);
    }
    let inputs = usize::from(raw[6]);
    let mut at = 7 + inputs * 41;
    let outputs = usize::from(raw[at]);
    at += 1;
    for _ in 0..outputs {
        at += 9 + usize::from(raw[at + 8]);
    }
    let mut stripped = raw[..4].to_vec();
    stripped.extend_from_slice(&raw[6..at]);
    stripped.extend_from_slice(&raw[raw.len() - 4..]);
    txid(&stripped)
}

pub async fn serve(log: Log, ledger: Arc<Mutex<Ledger>>) -> Url {
    crate::serve_lines(move |method, params| {
        log.lock()
            .unwrap()
            .push((method.to_owned(), params.clone()));
        ledger.lock().unwrap().answer(method, params)
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
