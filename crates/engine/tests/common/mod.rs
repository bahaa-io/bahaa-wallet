#![allow(dead_code)]

use std::fmt::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use adnl::AdnlPeer;
use adnl::crypto::{KeyPair, SecretKey};
use bahaa_config::{ChainKind, Checkpoint, Endpoint, Feature, Network, Protocol, Registry};
use bahaa_engine::EngineConfig;
use bahaa_market::MarketConfig;
use bahaa_vault::KdfParams;
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use bytes::Bytes;
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::watch;
use url::Url;

pub const HARDHAT: &str = "test test test test test test test test test test test junk";
pub const HARDHAT_0: &str = "0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266";
pub const HARDHAT_1: &str = "0x70997970C51812dc3A010C7d01b50e0d17dc79C8";
pub const USDT: &str = "0x55d398326f99059ff775485246999027b3197955";
pub const TRON_0: &str = "TWer2Ygk5TEheHp3TPuYeqxmB6SsGZmaL6";
pub const TRON_0_HEX: &str = "41e2e1a54926527fbb4e4420de4c6bab82beaee24d";
pub const TRON_GENESIS: &str = "00000000000000001ebf88508a03865c71d452e25f4d51194196a1d22b6653dc";

pub struct Scratch(pub PathBuf);

impl Scratch {
    pub fn new() -> Self {
        let mut random = [0u8; 8];
        getrandom::fill(&mut random).unwrap();
        Self(std::env::temp_dir().join(format!("bahaa-engine-{}", hex(&random))))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().fold(String::new(), |mut out, byte| {
        let _ = write!(out, "{byte:02x}");
        out
    })
}

pub fn result(value: Value) -> Value {
    let mut reply = json!({"jsonrpc": "2.0", "id": 1});
    reply["result"] = value;
    reply
}

pub fn failure(code: i64, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": 1, "error": {"code": code, "message": message}})
}

pub async fn node<F>(answer: F) -> Url
where
    F: Fn(&Value) -> (Value, Duration) + Send + Sync + 'static,
{
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
    let answer = Arc::new(answer);
    tokio::spawn(async move {
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            let answer = Arc::clone(&answer);
            tokio::spawn(async move {
                let Some((_, body)) = read_request(&mut socket).await else {
                    return;
                };
                let call: Value = serde_json::from_slice(&body).unwrap();
                let (reply, delay) = answer(&call);
                tokio::time::sleep(delay).await;
                let text = reply.to_string();
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{text}",
                    text.len()
                );
                let _ = socket.write_all(response.as_bytes()).await;
                let _ = socket.shutdown().await;
            });
        }
    });
    url
}

pub async fn rest_node<F>(answer: F) -> Url
where
    F: Fn(&str, &Value) -> Value + Send + Sync + 'static,
{
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
    let answer = Arc::new(answer);
    tokio::spawn(async move {
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            let answer = Arc::clone(&answer);
            tokio::spawn(async move {
                let Some((path, body)) = read_request(&mut socket).await else {
                    return;
                };
                let body = if body.is_empty() {
                    Value::Null
                } else {
                    serde_json::from_slice(&body).unwrap()
                };
                let text = answer(&path, &body).to_string();
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{text}",
                    text.len()
                );
                let _ = socket.write_all(response.as_bytes()).await;
                let _ = socket.shutdown().await;
            });
        }
    });
    url
}

async fn read_request(socket: &mut tokio::net::TcpStream) -> Option<(String, Vec<u8>)> {
    let mut request = Vec::new();
    let mut buffer = [0u8; 8192];
    loop {
        let read = socket.read(&mut buffer).await.unwrap_or(0);
        if read == 0 {
            return None;
        }
        request.extend_from_slice(&buffer[..read]);
        let text = String::from_utf8_lossy(&request).to_string();
        let Some(end) = text.find("\r\n\r\n") else {
            continue;
        };
        let length = text
            .lines()
            .find_map(|line| {
                line.to_ascii_lowercase()
                    .strip_prefix("content-length: ")
                    .map(str::to_owned)
            })
            .map_or(0, |value| value.trim().parse::<usize>().unwrap());
        if request.len() >= end + 4 + length {
            let path = text.split(' ').nth(1)?.to_owned();
            return Some((path, request[end + 4..end + 4 + length].to_vec()));
        }
    }
}

pub fn network(id: &str, url: Url) -> Network {
    let mut network = Registry::builtin()
        .unwrap()
        .get(&id.parse().unwrap())
        .unwrap()
        .clone();
    network.endpoints = vec![Endpoint {
        protocol: Protocol::JsonRpc,
        url,
        features: vec![Feature::Logs],
        min_gap_ms: 0,
    }];
    network
}

pub fn tron_network(url: Url) -> Network {
    let mut network = Registry::builtin()
        .unwrap()
        .get(&"tron".parse().unwrap())
        .unwrap()
        .clone();
    network.endpoints = vec![Endpoint {
        protocol: Protocol::TronHttp,
        url,
        features: Vec::new(),
        min_gap_ms: 0,
    }];
    network
}

pub fn config(scratch: &Scratch, registry: Registry, market: Option<MarketConfig>) -> EngineConfig {
    EngineConfig {
        data_dir: scratch.0.join("data"),
        kdf: KdfParams {
            memory_kib: 64,
            iterations: 1,
            lanes: 1,
        },
        market,
        registry: Some(registry),
        auto_lock_unit: Duration::from_secs(60),
        refresh_every: Duration::from_secs(60),
        draft_ttl: Duration::from_secs(120),
        track_every: Duration::from_millis(50),
    }
}

pub async fn until<T: Clone>(watcher: &mut watch::Receiver<T>, done: impl Fn(&T) -> bool) -> T {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            {
                let current = watcher.borrow_and_update();
                if done(&current) {
                    return current.clone();
                }
            }
            watcher.changed().await.unwrap();
        }
    })
    .await
    .unwrap()
}

pub type Asked = Arc<Mutex<Vec<(u32, String)>>>;

pub const GET_ACCOUNT_STATE: u32 = 0x6b89_0e25;
pub const GET_BLOCK_PROOF: u32 = 0x8aea_9c44;
const GET_MASTERCHAIN_INFO: u32 = 0x89b5_e62e;
const GET_CONFIG_PARAMS: u32 = 0x2a11_1c19;
const GET_LIBRARIES: u32 = 0xd122_b662;
const GET_TRANSACTIONS: u32 = 0x1c40_e7a1;
const KEY_BLOCK: u32 = 99_746_941;
const KEY_ROOT: &str = "09b48141aa77df65bc7b237ffe04fa540981b73d7e8ce14951ec8c03c24715b9";
const KEY_FILE: &str = "1d0561589eaf050d1cf9d8f145df05766d0adfa559570eb017dcaf92d8e7f48e";
pub const PREVIOUS_KEY: Checkpoint = Checkpoint {
    seqno: 99_727_067,
    root_hash: [
        0x5e, 0xf9, 0xe8, 0x82, 0xdc, 0x5e, 0x12, 0xa2, 0x8d, 0x0e, 0x17, 0xc9, 0x6a, 0xab, 0xb8,
        0xf5, 0x26, 0xd9, 0x93, 0x57, 0xb1, 0xc9, 0xfd, 0x81, 0x0f, 0x51, 0x22, 0x58, 0xa7, 0xf9,
        0x15, 0x74,
    ],
    file_hash: [
        0x28, 0x8b, 0x2b, 0x9d, 0x4d, 0xf2, 0xdb, 0xfe, 0xc8, 0x2c, 0x8b, 0xe5, 0x86, 0x25, 0x00,
        0x7e, 0xa5, 0xdc, 0x80, 0xa6, 0x75, 0x40, 0xc9, 0x00, 0x90, 0x6f, 0xff, 0xaa, 0x44, 0x63,
        0x92, 0x53,
    ],
};

pub fn unhex(text: &str) -> Vec<u8> {
    (0..text.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(&text[at..at + 2], 16).unwrap())
        .collect()
}

pub fn ton_network(id: &str, config: Url) -> Network {
    let mut network = Registry::builtin()
        .unwrap()
        .get(&id.parse().unwrap())
        .unwrap()
        .clone();
    network.endpoints = vec![Endpoint {
        protocol: Protocol::TonLiteservers,
        url: config,
        features: Vec::new(),
        min_gap_ms: 0,
    }];
    if let ChainKind::Ton { checkpoint, .. } = &mut network.kind
        && id == "ton"
    {
        *checkpoint = Checkpoint {
            seqno: KEY_BLOCK,
            root_hash: unhex(KEY_ROOT).try_into().unwrap(),
            file_hash: unhex(KEY_FILE).try_into().unwrap(),
        };
    }
    network
}

fn tl_bytes(out: &mut Vec<u8>, value: &[u8]) {
    let start = out.len();
    if value.len() < 254 {
        out.push(u8::try_from(value.len()).unwrap());
    } else {
        out.push(0xfe);
        out.extend_from_slice(&u32::try_from(value.len()).unwrap().to_le_bytes()[..3]);
    }
    out.extend_from_slice(value);
    while !(out.len() - start).is_multiple_of(4) {
        out.push(0);
    }
}

fn read_tl_bytes(data: &[u8]) -> &[u8] {
    let (length, start) = if data[0] < 254 {
        (usize::from(data[0]), 1)
    } else {
        (
            usize::from(data[1]) | usize::from(data[2]) << 8 | usize::from(data[3]) << 16,
            4,
        )
    };
    &data[start..start + length]
}

fn refusal() -> Vec<u8> {
    let mut out = 0xbba9_e148_u32.to_le_bytes().to_vec();
    out.extend_from_slice(&(-400i32).to_le_bytes());
    tl_bytes(&mut out, b"unknown");
    out
}

fn replay(recording: &Value, data: &[u8], asked: &Asked) -> Vec<u8> {
    let function = u32::from_le_bytes(data[..4].try_into().unwrap());
    let recorded = |path: &[&str]| {
        path.iter()
            .fold(recording, |value, key| &value[*key])
            .as_str()
            .map(unhex)
    };
    let (key, answer) = match function {
        GET_MASTERCHAIN_INFO => (String::new(), recorded(&["masterchain_info"])),
        GET_BLOCK_PROOF => {
            let known = i32::from_le_bytes(data[20..24].try_into().unwrap());
            let answer = if known == 99_727_067 {
                recorded(&["block_proof_from_previous"])
            } else {
                recorded(&["block_proof"])
            };
            (known.to_string(), answer)
        }
        GET_CONFIG_PARAMS => (String::new(), recorded(&["config"])),
        GET_ACCOUNT_STATE => {
            let hash = hex(&data[88..120]);
            let answer = recorded(&["accounts", &hash]);
            (hash, answer)
        }
        GET_LIBRARIES => {
            let hash = hex(&data[8..40]);
            let answer = recorded(&["libraries", &hash]);
            (hash, answer)
        }
        GET_TRANSACTIONS => {
            let lt = i64::from_le_bytes(data[44..52].try_into().unwrap()).to_string();
            let answer = recorded(&["transactions", &lt]);
            (lt, answer)
        }
        _ => (String::new(), None),
    };
    asked.lock().unwrap().push((function, key));
    answer.unwrap_or_else(refusal)
}

pub async fn liteserver(recording: &'static str, zero_state: &str, asked: &Asked) -> Url {
    let recording: Arc<Value> = Arc::new(serde_json::from_str(recording).unwrap());
    let keys = KeyPair::from(&SecretKey::from_bytes([1; 32]));
    let public = keys.public_key.to_bytes();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let asked = Arc::clone(asked);
    tokio::spawn(async move {
        loop {
            let (socket, _) = listener.accept().await.unwrap();
            let (recording, asked) = (Arc::clone(&recording), Arc::clone(&asked));
            tokio::spawn(async move {
                let Ok(mut peer) = AdnlPeer::handle_handshake(socket, |_| Some(keys)).await else {
                    return;
                };
                while let Some(Ok(frame)) = peer.next().await {
                    let id = frame[4..36].to_vec();
                    let query = read_tl_bytes(&frame[36..]);
                    let data = read_tl_bytes(&query[4..]);
                    let answer = replay(&recording, data, &asked);
                    let mut out = 0x0fac_8416_u32.to_le_bytes().to_vec();
                    out.extend_from_slice(&id);
                    tl_bytes(&mut out, &answer);
                    if peer.send(Bytes::from(out)).await.is_err() {
                        return;
                    }
                }
            });
        }
    });
    let list = json!({
        "liteservers": [{"ip": 2_130_706_433, "port": port, "id": {"@type": "pub.ed25519", "key": STANDARD.encode(public)}}],
        "validator": {"zero_state": {"root_hash": STANDARD.encode(unhex(zero_state))}}
    });
    rest_node(move |_, _| list.clone()).await
}
