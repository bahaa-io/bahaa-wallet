#![allow(dead_code)]

use std::fmt::Write;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use bahaa_config::{Endpoint, Feature, Network, Protocol, Registry};
use bahaa_engine::EngineConfig;
use bahaa_market::MarketConfig;
use bahaa_vault::KdfParams;
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
                let text = answer(&path, &serde_json::from_slice(&body).unwrap()).to_string();
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
