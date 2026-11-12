#![allow(clippy::unwrap_used)]

pub mod anvil;
pub mod electrum;

use std::fmt::Write as _;
use std::sync::Arc;
use std::time::Duration;

use bahaa_chain::{ChainError, Failure};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use url::Url;

pub struct Request {
    pub head: String,
    pub body: Vec<u8>,
}

impl Request {
    pub fn path(&self) -> &str {
        self.head.split(' ').nth(1).unwrap_or("/")
    }

    pub fn json(&self) -> Value {
        if self.body.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&self.body).unwrap()
        }
    }

    pub fn text(&self) -> String {
        format!("{}{}", self.head, String::from_utf8_lossy(&self.body))
    }
}

pub struct Reply {
    pub status: u16,
    pub headers: Vec<(&'static str, String)>,
    pub body: Vec<u8>,
    pub delay: Duration,
}

impl Reply {
    pub fn json(status: u16, value: &Value) -> Self {
        Self::bytes(status, "application/json", value.to_string().into_bytes())
    }

    pub fn bytes(status: u16, kind: &str, body: Vec<u8>) -> Self {
        Self {
            status,
            headers: vec![("Content-Type", kind.to_owned())],
            body,
            delay: Duration::ZERO,
        }
    }
}

pub async fn serve(answer: impl Fn(Request) -> Reply + Send + Sync + 'static) -> Url {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
    let answer = Arc::new(answer);
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            let answer = Arc::clone(&answer);
            tokio::spawn(async move {
                let Some(request) = read(&mut socket).await else {
                    return;
                };
                let reply = answer(request);
                tokio::time::sleep(reply.delay).await;
                let mut head = format!("HTTP/1.1 {} X\r\n", reply.status);
                for (name, value) in &reply.headers {
                    let _ = write!(head, "{name}: {value}\r\n");
                }
                let _ = write!(
                    head,
                    "Content-Length: {}\r\nConnection: close\r\n\r\n",
                    reply.body.len()
                );
                let _ = socket.write_all(head.as_bytes()).await;
                let _ = socket.write_all(&reply.body).await;
                let _ = socket.shutdown().await;
            });
        }
    });
    url
}

pub async fn serve_lines(
    answer: impl Fn(&str, &Value) -> Result<Value, String> + Send + Sync + 'static,
) -> Url {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = Url::parse(&format!("tcp://{}", listener.local_addr().unwrap())).unwrap();
    let answer = Arc::new(answer);
    tokio::spawn(async move {
        while let Ok((socket, _)) = listener.accept().await {
            let answer = Arc::clone(&answer);
            tokio::spawn(async move {
                let (reader, mut writer) = socket.into_split();
                let mut lines = BufReader::new(reader).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    let request: Value = serde_json::from_str(&line).unwrap();
                    let reply = match &request {
                        Value::Array(batch) => Value::Array(
                            batch.iter().rev().map(|one| reply(&*answer, one)).collect(),
                        ),
                        one => reply(&*answer, one),
                    };
                    let mut text = reply.to_string();
                    text.push('\n');
                    if writer.write_all(text.as_bytes()).await.is_err() {
                        return;
                    }
                }
            });
        }
    });
    url
}

fn reply(answer: &dyn Fn(&str, &Value) -> Result<Value, String>, request: &Value) -> Value {
    let id = &request["id"];
    match answer(request["method"].as_str().unwrap(), &request["params"]) {
        Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
        Err(message) => {
            json!({"jsonrpc": "2.0", "id": id, "error": {"code": 1, "message": message}})
        }
    }
}

async fn read(socket: &mut TcpStream) -> Option<Request> {
    let mut request = Vec::new();
    let mut buffer = [0u8; 8192];
    loop {
        let read = socket.read(&mut buffer).await.ok()?;
        if read == 0 {
            return None;
        }
        request.extend_from_slice(&buffer[..read]);
        let text = String::from_utf8_lossy(&request);
        let Some(end) = text.find("\r\n\r\n") else {
            continue;
        };
        let length = text
            .lines()
            .find_map(|line| {
                line.to_ascii_lowercase()
                    .strip_prefix("content-length: ")
                    .and_then(|value| value.trim().parse::<usize>().ok())
            })
            .unwrap_or(0);
        if request.len() >= end + 4 + length {
            return Some(Request {
                head: text[..end + 4].to_owned(),
                body: request[end + 4..end + 4 + length].to_vec(),
            });
        }
    }
}

pub fn kind(failure: &Failure<ChainError>) -> &'static str {
    match failure {
        Failure::Retry(_) => "retry",
        Failure::RateLimited(..) => "limited",
        Failure::Unreachable(_) => "unreachable",
        Failure::Unsupported(ChainError::Pruned) => "pruned",
        Failure::Unsupported(_) => "unsupported",
        Failure::Fatal(_) => "fatal",
    }
}
