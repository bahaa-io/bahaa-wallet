#![allow(dead_code)]

use std::fmt::Write;
use std::sync::{Arc, Mutex};

use bahaa_config::{ChainKind, Checkpoint, Network};
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use url::Url;

pub type Log = Arc<Mutex<Vec<String>>>;

pub const BLOCKS: &str = include_str!("../fixtures/blocks.json");
const KEY_BLOCK: u32 = 99_746_941;
const KEY_ROOT: &str = "09b48141aa77df65bc7b237ffe04fa540981b73d7e8ce14951ec8c03c24715b9";
const KEY_FILE: &str = "1d0561589eaf050d1cf9d8f145df05766d0adfa559570eb017dcaf92d8e7f48e";

pub fn pinned(mut network: Network) -> Network {
    if let ChainKind::Ton { checkpoint, .. } = &mut network.kind {
        *checkpoint = Checkpoint {
            seqno: KEY_BLOCK,
            root_hash: unhex(KEY_ROOT).try_into().unwrap(),
            file_hash: unhex(KEY_FILE).try_into().unwrap(),
        };
    }
    network
}

pub fn anchor_proof(masterchain_info: &[u8]) -> Vec<u8> {
    let seqno = i32::from_le_bytes(masterchain_info[16..20].try_into().unwrap());
    recorded(BLOCKS, &["anchors", &seqno.to_string()]).unwrap()
}

pub fn unhex(text: &str) -> Vec<u8> {
    (0..text.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(&text[at..at + 2], 16).unwrap())
        .collect()
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().fold(String::new(), |mut out, byte| {
        let _ = write!(out, "{byte:02x}");
        out
    })
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
            let (log, reply) = (Arc::clone(&log), Arc::clone(&reply));
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
                let body = if body.is_empty() {
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

pub fn read_bytes(data: &[u8]) -> (&[u8], usize) {
    let (length, start) = if data[0] < 254 {
        (usize::from(data[0]), 1)
    } else {
        (
            usize::from(data[1]) | usize::from(data[2]) << 8 | usize::from(data[3]) << 16,
            4,
        )
    };
    let used = start + length;
    (&data[start..used], used + (4 - used % 4) % 4)
}

pub fn recorded(recording: &str, path: &[&str]) -> Option<Vec<u8>> {
    let fixture: Value = serde_json::from_str(recording).unwrap();
    path.iter()
        .fold(&fixture, |value, key| &value[*key])
        .as_str()
        .map(unhex)
}
