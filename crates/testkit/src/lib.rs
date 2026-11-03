#![allow(clippy::unwrap_used)]

use std::fmt::Write as _;
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
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
    pub body: String,
    pub delay: Duration,
}

impl Reply {
    pub fn json(status: u16, value: &Value) -> Self {
        Self {
            status,
            headers: vec![("Content-Type", "application/json".to_owned())],
            body: value.to_string(),
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
                let _ = socket.write_all(reply.body.as_bytes()).await;
                let _ = socket.shutdown().await;
            });
        }
    });
    url
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
