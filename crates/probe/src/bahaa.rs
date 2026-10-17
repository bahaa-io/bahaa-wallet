use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use futures_util::{SinkExt, StreamExt};
use reqwest::StatusCode;
use reqwest::header::{AsHeaderName, ETAG, HeaderMap, IF_NONE_MATCH};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::value::RawValue;
use serde_json::{Value, json};
use tokio::time::{Instant, timeout_at};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{Connector, connect_async_tls_with_config};

use crate::clock::unix_now;
use crate::net::Net;
use crate::report::{self, Outcome, Report};

const API: &str = "https://api.bahaa.io/v1";
const STREAM: &str = "wss://api.bahaa.io/v1/stream";
const SYMBOLS: [&str; 8] = ["BNB", "ETH", "POLYG", "AVAX", "TRX", "GRAM", "USDT", "USDC"];
const LISTEN: Duration = Duration::from_secs(20);

#[derive(Deserialize)]
struct Envelope<T> {
    ok: bool,
    code: Option<String>,
    description: Option<String>,
    result: Option<T>,
}

#[derive(Deserialize)]
struct Rate {
    symbol: String,
    name: String,
    prices: BTreeMap<String, Box<RawValue>>,
    updated_at_unix: u64,
}

#[derive(Deserialize)]
struct Frame {
    #[serde(rename = "type")]
    kind: String,
    count: Option<u64>,
    result: Option<Vec<Value>>,
    code: Option<String>,
    description: Option<String>,
}

struct Reply<T> {
    result: T,
    headers: HeaderMap,
    bytes: usize,
}

pub async fn run(net: &Net, report: &mut Report) {
    report::section("bahaa");
    let mut etag = None;
    report.check("rates", rates(net, &mut etag)).await;
    report
        .check("rates etag", revalidate(net, etag.as_deref()))
        .await;
    report.check("stream", Box::pin(stream(net))).await;
}

async fn rates(net: &Net, etag: &mut Option<String>) -> Result<Outcome> {
    let reply: Reply<Vec<Rate>> =
        call(net.http.post(format!("{API}/rates")).json(&query())).await?;
    *etag = header(&reply.headers, ETAG);
    let missing: Vec<&str> = SYMBOLS
        .into_iter()
        .filter(|symbol| !reply.result.iter().any(|rate| rate.symbol == *symbol))
        .collect();
    if !missing.is_empty() {
        bail!("missing from the feed: {}", missing.join(", "));
    }
    let limit = header(&reply.headers, "x-ratelimit-limit").unwrap_or_default();
    let left = header(&reply.headers, "x-ratelimit-remaining").unwrap_or_default();
    let now = unix_now()?;
    let mut outcome = Outcome::new(format!(
        "{} symbols, {} bytes, {left} of {limit} calls left this minute",
        reply.result.len(),
        reply.bytes
    ));
    for rate in &reply.result {
        let price = rate.prices.get("USDT").map_or("none", |price| price.get());
        let age = now.saturating_sub(rate.updated_at_unix);
        outcome.line(format!(
            "{:<6} {:<24} {price:>22} USDT  {age:>4} s old",
            rate.symbol, rate.name
        ));
    }
    Ok(outcome)
}

async fn revalidate(net: &Net, etag: Option<&str>) -> Result<Outcome> {
    let etag = etag.context("the rates reply had no ETag")?;
    let response = net
        .http
        .post(format!("{API}/rates"))
        .header(IF_NONE_MATCH, etag)
        .json(&query())
        .send()
        .await?;
    let status = response.status();
    if status == StatusCode::NOT_MODIFIED {
        return Ok(Outcome::new(format!("304, nothing changed since {etag}")));
    }
    if !status.is_success() {
        bail!("HTTP {status}");
    }
    let fresh = header(response.headers(), ETAG).unwrap_or_default();
    Ok(Outcome::new(format!(
        "{status}, snapshot already moved from {etag} to {fresh}"
    )))
}

async fn stream(net: &Net) -> Result<Outcome> {
    let started = Instant::now();
    let connector = Connector::Rustls(Arc::clone(&net.tls));
    let (mut socket, _) =
        connect_async_tls_with_config(STREAM, None, false, Some(connector)).await?;
    let connected = started.elapsed();
    socket
        .send(text(&json!({
            "type": "subscribe",
            "id": "probe",
            "condition": condition(),
            "bases": ["USDT"],
            "snapshot": true,
        })))
        .await?;
    let mut subscribed = None;
    let mut snapshot = None;
    let mut updates = 0;
    let mut prices = 0;
    let mut pings = 0;
    while let Ok(Some(message)) = timeout_at(started + LISTEN, socket.next()).await {
        let Message::Text(body) = message? else {
            continue;
        };
        let frame: Frame = serde_json::from_str(&body)?;
        match frame.kind.as_str() {
            "subscribed" => subscribed = Some((started.elapsed(), frame.count.unwrap_or(0))),
            "snapshot" => snapshot = Some((started.elapsed(), items(&frame))),
            "rates" => {
                updates += 1;
                prices += items(&frame);
            }
            "ping" => {
                pings += 1;
                socket.send(text(&json!({"type": "pong"}))).await?;
            }
            "error" => bail!(
                "{}: {}",
                frame.code.unwrap_or_default(),
                frame.description.unwrap_or_default()
            ),
            _ => {}
        }
    }
    socket
        .send(text(&json!({"type": "unsubscribe", "id": "probe"})))
        .await?;
    socket.close(None).await?;
    let (subscribed_at, count) = subscribed.context("no subscribed frame")?;
    let (snapshot_at, snapshot_items) = snapshot.context("no snapshot frame")?;
    if snapshot_items != SYMBOLS.len() {
        bail!(
            "snapshot had {snapshot_items} items, expected {}",
            SYMBOLS.len()
        );
    }
    let mut outcome = Outcome::new(format!(
        "{updates} updates with {prices} prices in {} s",
        LISTEN.as_secs()
    ));
    outcome.line(format!(
        "connected {} ms, subscribed to {count} at {} ms, snapshot at {} ms",
        connected.as_millis(),
        subscribed_at.as_millis(),
        snapshot_at.as_millis()
    ));
    outcome.line(format!("{pings} server pings answered"));
    Ok(outcome)
}

fn condition() -> Value {
    json!({
        "operation": "and",
        "conditions": [
            {"operation": "equals", "left": "category", "right": "crypto"},
            {"operation": "in", "left": "symbol", "right": SYMBOLS},
        ],
    })
}

fn query() -> Value {
    json!({"condition": condition(), "bases": ["USDT"]})
}

async fn call<T: DeserializeOwned>(request: reqwest::RequestBuilder) -> Result<Reply<T>> {
    let response = request.send().await?;
    let status = response.status();
    let headers = response.headers().clone();
    let body = response.bytes().await?;
    let envelope: Envelope<T> =
        serde_json::from_slice(&body).with_context(|| format!("HTTP {status}"))?;
    if !envelope.ok {
        bail!(
            "{}: {}",
            envelope.code.unwrap_or_default(),
            envelope.description.unwrap_or_default()
        );
    }
    Ok(Reply {
        result: envelope.result.context("ok reply without a result")?,
        headers,
        bytes: body.len(),
    })
}

fn header(headers: &HeaderMap, name: impl AsHeaderName) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
}

fn items(frame: &Frame) -> usize {
    frame.result.as_ref().map_or(0, Vec::len)
}

fn text(value: &Value) -> Message {
    Message::text(value.to_string())
}
