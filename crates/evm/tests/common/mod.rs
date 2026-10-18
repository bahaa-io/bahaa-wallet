#![allow(dead_code)]

use std::sync::{Arc, Mutex};

use bahaa_config::{Endpoint, Feature, Network, Protocol, Registry};
use bahaa_types::{Address, AssetId, ChainFamily};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
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
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
    let reply = Arc::new(reply);
    tokio::spawn(async move {
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            let log = Arc::clone(&log);
            let reply = Arc::clone(&reply);
            tokio::spawn(async move {
                let mut request = Vec::new();
                let mut buffer = [0u8; 8192];
                let body = loop {
                    let read = socket.read(&mut buffer).await.unwrap_or(0);
                    if read == 0 {
                        return;
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
                        break request[end + 4..end + 4 + length].to_vec();
                    }
                };
                let call: Value = serde_json::from_slice(&body).unwrap();
                let method = call["method"].as_str().unwrap().to_owned();
                log.lock().unwrap().push(method.clone());
                let (status, answer) = reply(&method, &call["params"]);
                let text = answer.to_string();
                let response = format!(
                    "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{text}",
                    text.len()
                );
                let _ = socket.write_all(response.as_bytes()).await;
                let _ = socket.shutdown().await;
            });
        }
    });
    url
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
