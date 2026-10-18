#![allow(dead_code)]

use std::fmt::Write;
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use url::Url;

pub type Log = Arc<Mutex<Vec<String>>>;

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

pub fn block_json(block: &[u8]) -> Value {
    let int = |at: usize| i32::from_le_bytes(block[at..at + 4].try_into().unwrap());
    let shard = u64::from_le_bytes(block[4..12].try_into().unwrap());
    json!({
        "workchain": int(0),
        "shard": format!("{shard:016x}"),
        "seqno": int(12),
        "root_hash": hex(&block[16..48]),
        "file_hash": hex(&block[48..80]),
    })
}

pub fn proxied_head(info: &[u8], zero_state: &str) -> Value {
    json!({"last": block_json(&info[4..84]), "init": {"root_hash": zero_state}})
}

pub fn proxied_account(answer: &[u8]) -> Value {
    let (shard_proof, first) = read_bytes(&answer[164..]);
    let (proof, second) = read_bytes(&answer[164 + first..]);
    let (state, _) = read_bytes(&answer[164 + first + second..]);
    json!({
        "id": block_json(&answer[4..84]),
        "shardblk": block_json(&answer[84..164]),
        "shard_proof": hex(shard_proof),
        "proof": hex(proof),
        "state": hex(state),
    })
}

pub fn proxied_config(answer: &[u8]) -> Value {
    let (state_proof, first) = read_bytes(&answer[88..]);
    let (config_proof, _) = read_bytes(&answer[88 + first..]);
    json!({
        "id": block_json(&answer[8..88]),
        "state_proof": hex(state_proof),
        "config_proof": hex(config_proof),
    })
}

pub fn proxied_library(answer: &[u8]) -> Value {
    let (data, _) = read_bytes(&answer[40..]);
    json!({"boc": hex(data)})
}
