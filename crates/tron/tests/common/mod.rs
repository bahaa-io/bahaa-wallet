#![allow(dead_code)]

use std::sync::{Arc, Mutex};

use bahaa_config::{Endpoint, Network, Protocol, Registry};
use bahaa_types::{Address, AssetId, ChainFamily};
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
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
                let (path, body) = loop {
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
                        let path = text.split(' ').nth(1).unwrap().to_owned();
                        break (path, request[end + 4..end + 4 + length].to_vec());
                    }
                };
                let body: Value = if body.is_empty() {
                    Value::Null
                } else {
                    serde_json::from_slice(&body).unwrap()
                };
                log.lock().unwrap().push(path.clone());
                let (status, answer) = reply(&path, &body);
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
