use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use rustls::ClientConfig;
use rustls_platform_verifier::ConfigVerifierExt;
use serde_json::Value;
use tokio::time::{Instant, sleep_until};

const ERROR_BODY: usize = 100;

pub struct Net {
    pub http: reqwest::Client,
    pub tls: Arc<ClientConfig>,
}

impl Net {
    pub fn new() -> Result<Self> {
        let http = reqwest::Client::builder()
            .user_agent(concat!("bahaa-probe/", env!("CARGO_PKG_VERSION")))
            .timeout(Duration::from_secs(15))
            .build()?;
        let tls = Arc::new(ClientConfig::with_platform_verifier()?);
        Ok(Self { http, tls })
    }
}

pub struct Paced<'a> {
    net: &'a Net,
    base: &'a str,
    gap: Duration,
    next: Instant,
}

impl<'a> Paced<'a> {
    pub fn new(net: &'a Net, base: &'a str, gap: Duration) -> Self {
        Self {
            net,
            base,
            gap,
            next: Instant::now(),
        }
    }

    pub async fn get(&mut self, path: &str) -> Result<Value> {
        let request = self.net.http.get(format!("{}{path}", self.base));
        self.send(request).await
    }

    pub async fn post(&mut self, path: &str, body: &Value) -> Result<Value> {
        let request = self
            .net
            .http
            .post(format!("{}{path}", self.base))
            .json(body);
        self.send(request).await
    }

    pub async fn expect(&mut self, path: &str, pointer: &str) -> Result<String> {
        let reply = self.get(path).await?;
        match reply.pointer(pointer) {
            None | Some(Value::Null | Value::Bool(false)) => bail!("reply has no {pointer}"),
            Some(_) => Ok("yes".to_owned()),
        }
    }

    async fn send(&mut self, request: reqwest::RequestBuilder) -> Result<Value> {
        sleep_until(self.next).await;
        let reply = fetch(request).await;
        self.next = Instant::now() + self.gap;
        reply
    }
}

async fn fetch(request: reqwest::RequestBuilder) -> Result<Value> {
    let response = request.send().await?;
    let status = response.status();
    let bytes = response.bytes().await?;
    if !status.is_success() {
        let body: String = String::from_utf8_lossy(&bytes)
            .trim()
            .chars()
            .take(ERROR_BODY)
            .collect();
        bail!("HTTP {status}: {body}");
    }
    serde_json::from_slice(&bytes).with_context(|| format!("HTTP {status}"))
}
