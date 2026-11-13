use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bahaa_types::{Address, BitcoinScript, hex};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use url::Url;

use crate::{Reply, Request};

pub type Log = Arc<Mutex<Vec<(String, Value)>>>;

fn locking(address: &Address) -> Vec<u8> {
    let Address::Bitcoin { script, .. } = address else {
        panic!("the fake Electrum server holds Bitcoin addresses");
    };
    match script {
        BitcoinScript::PubkeyHash(hash) => [&[0x76, 0xa9, 0x14][..], hash, &[0x88, 0xac]].concat(),
        BitcoinScript::ScriptHash(hash) => [&[0xa9, 0x14][..], hash, &[0x87]].concat(),
        BitcoinScript::WitnessKeyHash(hash) => [&[0x00, 0x14][..], hash].concat(),
        BitcoinScript::WitnessScriptHash(hash) => [&[0x00, 0x20][..], hash].concat(),
        BitcoinScript::Taproot(key) => [&[0x51, 0x20][..], key].concat(),
    }
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
    let script = locking(to);
    raw.push(u8::try_from(script.len()).unwrap());
    raw.extend_from_slice(&script);
    raw.extend_from_slice(&[0, 0, 0, 0]);
    (txid(&raw), hex::encode(&raw))
}

pub const CHECKPOINT: u64 = 899_136;
pub const POW_LIMIT: u32 = 0x207f_ffff;

const TARGET: [u8; 3] = [0x7f, 0xff, 0xff];

pub fn checkpoint() -> [u8; 32] {
    hex::decode_array(&txid(&Ledger::default().header(CHECKPOINT))).unwrap()
}

fn sha256d(data: &[u8]) -> [u8; 32] {
    Sha256::digest(Sha256::digest(data)).into()
}

fn internal(display: &str) -> [u8; 32] {
    let mut bytes: [u8; 32] = hex::decode_array(display).unwrap();
    bytes.reverse();
    bytes
}

fn levels(ids: &[[u8; 32]]) -> Vec<Vec<[u8; 32]>> {
    let mut levels = vec![ids.to_vec()];
    while levels.last().unwrap().len() > 1 {
        let level = levels.last().unwrap();
        let next = level
            .chunks(2)
            .map(|pair| sha256d(&[pair[0], *pair.get(1).unwrap_or(&pair[0])].concat()))
            .collect();
        levels.push(next);
    }
    levels
}

fn mine(previous: [u8; 32], root: [u8; 32], height: u64) -> [u8; 80] {
    let mut raw = [0u8; 80];
    raw[..4].copy_from_slice(&0x2000_0000u32.to_le_bytes());
    raw[4..36].copy_from_slice(&previous);
    raw[36..68].copy_from_slice(&root);
    raw[68..72].copy_from_slice(&u32::try_from(time_of(height)).unwrap().to_le_bytes());
    raw[72..76].copy_from_slice(&POW_LIMIT.to_le_bytes());
    for nonce in 0u32.. {
        raw[76..80].copy_from_slice(&nonce.to_le_bytes());
        let mut hash = sha256d(&raw);
        hash.reverse();
        if hash[..3] <= TARGET[..] {
            break;
        }
    }
    raw
}

#[derive(Clone, Debug, Default)]
pub struct Ledger {
    pub genesis: String,
    pub histories: HashMap<String, Vec<Value>>,
    pub unspent: HashMap<String, Vec<Value>>,
    pub transactions: HashMap<String, String>,
    pub broadcast: Vec<String>,
    pub refusal: Option<String>,
    pub unestimated: Vec<u64>,
    pub rates: HashMap<u64, String>,
    pub forge_proofs: bool,
    blocks: HashMap<String, u64>,
    headers: HashMap<u64, [u8; 80]>,
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
        self.touched(height);
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
        self.touched(height);
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
            "blockchain.block.header" => {
                json!(hex::encode(&self.header(params[0].as_u64().unwrap())))
            }
            "blockchain.block.headers" => {
                let start = params[0].as_u64().unwrap();
                let end = (start + params[1].as_u64().unwrap()).min(self.tip() + 1);
                let headers: Vec<u8> = (start..end)
                    .flat_map(|height| self.header(height))
                    .collect();
                json!({"count": end.saturating_sub(start), "hex": hex::encode(&headers), "max": 2016})
            }
            "blockchain.transaction.get_merkle" => self.proof(&key, params[1].as_u64().unwrap()),
            "blockchain.estimatefee" if self.unestimated.contains(&params[0].as_u64().unwrap()) => {
                return Err(format!("cannot estimate fee for {} blocks", params[0]));
            }
            "blockchain.estimatefee" => {
                let target = params[0].as_u64().unwrap();
                let rate = self
                    .rates
                    .get(&target)
                    .map_or(if target <= 6 { "2e-05" } else { "5e-06" }, String::as_str);
                serde_json::from_str(rate).unwrap()
            }
            "blockchain.relayfee" => serde_json::from_str("1e-06").unwrap(),
            "blockchain.transaction.broadcast" => json!(self.accept(&key)?),
            "blockchain.transaction.get" => match self.transactions.get(&key) {
                Some(raw) => json!(raw),
                None => return Err("missing transaction".to_owned()),
            },
            other => return Err(format!("unknown method {other}")),
        })
    }
}

impl Ledger {
    fn touched(&mut self, height: u64) {
        self.headers.retain(|known, _| *known < height);
    }

    fn entries(&self) -> impl Iterator<Item = &Value> {
        self.histories
            .values()
            .chain(self.unspent.values())
            .flatten()
    }

    fn tip(&self) -> u64 {
        self.entries()
            .filter_map(|entry| entry["height"].as_u64())
            .max()
            .unwrap_or(0)
            .max(CHECKPOINT)
    }

    fn block(&self, height: u64) -> Vec<[u8; 32]> {
        let mut ids: Vec<String> = self
            .entries()
            .filter(|entry| entry["height"].as_u64() == Some(height))
            .map(|entry| entry["tx_hash"].as_str().unwrap().to_owned())
            .collect();
        ids.sort();
        ids.dedup();
        if ids.is_empty() {
            return vec![sha256d(&height.to_le_bytes())];
        }
        ids.iter().map(|id| internal(id)).collect()
    }

    fn header(&mut self, height: u64) -> [u8; 80] {
        if let Some(known) = self.headers.get(&height) {
            return *known;
        }
        let start = if height > CHECKPOINT {
            (CHECKPOINT..height)
                .rev()
                .find(|known| self.headers.contains_key(known))
                .map_or(CHECKPOINT, |known| known + 1)
        } else {
            height
        };
        for at in start..=height {
            let previous = if at > CHECKPOINT {
                sha256d(&self.headers[&(at - 1)])
            } else {
                [0; 32]
            };
            let root = levels(&self.block(at)).last().unwrap()[0];
            self.headers.insert(at, mine(previous, root, at));
        }
        self.headers[&height]
    }

    fn proof(&self, id: &str, height: u64) -> Value {
        let ids = self.block(height);
        let wanted = internal(id);
        let pos = ids.iter().position(|known| *known == wanted).unwrap();
        let levels = levels(&ids);
        let mut at = pos;
        let mut merkle = Vec::new();
        for level in &levels[..levels.len() - 1] {
            let mut sibling = *level.get(at ^ 1).unwrap_or(&level[at]);
            sibling.reverse();
            sibling[0] ^= u8::from(self.forge_proofs);
            merkle.push(hex::encode(&sibling));
            at /= 2;
        }
        json!({"block_height": height, "merkle": merkle, "pos": pos})
    }

    fn accept(&mut self, raw: &str) -> Result<String, String> {
        if let Some(refusal) = &self.refusal {
            return Err(refusal.clone());
        }
        let id = witnessless_id(&hex::decode(raw).unwrap());
        self.broadcast.push(raw.to_owned());
        self.transactions.insert(id.clone(), raw.to_owned());
        Ok(id)
    }

    pub fn esplora(&mut self, request: &Request) -> Reply {
        let text = |status, body: String| Reply {
            status,
            headers: Vec::new(),
            body: body.into_bytes(),
            delay: Duration::ZERO,
        };
        let path = request.path().trim_start_matches('/').to_owned();
        let parts: Vec<&str> = path.split('/').collect();
        let electrum_key = |plain: &str| {
            let mut bytes = hex::decode(plain).unwrap();
            bytes.reverse();
            hex::encode(&bytes)
        };
        let status = |height: i64| {
            if height > 0 {
                json!({"confirmed": true, "block_height": height})
            } else {
                json!({"confirmed": false})
            }
        };
        match parts.as_slice() {
            ["tx"] => match self.accept(&String::from_utf8_lossy(&request.body)) {
                Ok(id) => text(200, id),
                Err(refusal) => text(400, refusal),
            },
            ["block-height", height] => {
                let height: u64 = height.parse().unwrap();
                if height == 0 {
                    return text(200, self.genesis.clone());
                }
                let hash = txid(&self.header(height));
                self.blocks.insert(hash.clone(), height);
                text(200, hash)
            }
            ["block", hash, "header"] => match self.blocks.get(*hash).copied() {
                Some(height) => text(200, hex::encode(&self.header(height))),
                None => text(404, String::new()),
            },
            ["tx", id, "merkle-proof"] => {
                let height = self
                    .entries()
                    .find(|entry| entry["tx_hash"] == *id && entry["height"].as_u64() > Some(0))
                    .and_then(|entry| entry["height"].as_u64());
                match height {
                    Some(height) => Reply::json(200, &self.proof(id, height)),
                    None => text(404, String::new()),
                }
            }
            ["scripthash", hash, "txs"] => {
                let seen = self
                    .histories
                    .get(&electrum_key(hash))
                    .cloned()
                    .unwrap_or_default();
                let listed: Vec<Value> = seen
                    .iter()
                    .rev()
                    .map(|entry| {
                        json!({"txid": entry["tx_hash"], "status": status(entry["height"].as_i64().unwrap())})
                    })
                    .collect();
                Reply::json(200, &json!(listed))
            }
            ["scripthash", _, "txs", "chain", _] => Reply::json(200, &json!([])),
            ["scripthash", hash, "utxo"] => {
                let coins = self
                    .unspent
                    .get(&electrum_key(hash))
                    .cloned()
                    .unwrap_or_default();
                let listed: Vec<Value> = coins
                    .iter()
                    .map(|coin| {
                        json!({"txid": coin["tx_hash"], "vout": coin["tx_pos"], "value": coin["value"], "status": status(coin["height"].as_i64().unwrap())})
                    })
                    .collect();
                Reply::json(200, &json!(listed))
            }
            ["tx", id, "hex"] => match self.transactions.get(*id) {
                Some(raw) => text(200, raw.clone()),
                None => text(404, "Transaction not found".to_owned()),
            },
            ["fee-estimates"] => text(200, "{\"1\": 3.5, \"2\": 2.0}".to_owned()),
            _ => text(404, String::new()),
        }
    }
}

pub async fn serve_esplora(log: Log, ledger: Arc<Mutex<Ledger>>) -> Url {
    crate::serve(move |request| {
        log.lock()
            .unwrap()
            .push((request.path().to_owned(), Value::Null));
        ledger.lock().unwrap().esplora(&request)
    })
    .await
}

fn witnessless_id(raw: &[u8]) -> String {
    if raw[4] != 0 {
        return txid(raw);
    }
    let mut at = 7;
    for _ in 0..raw[6] {
        at += 36;
        at += 1 + usize::from(raw[at]) + 4;
    }
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
