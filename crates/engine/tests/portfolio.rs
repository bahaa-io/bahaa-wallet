#![allow(clippy::unwrap_used)]

use std::fmt::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bahaa_config::{Endpoint, Feature, Fiat, Network, Protocol, Registry, Settings};
use bahaa_crypto::{SecretString, WordCount};
use bahaa_engine::{Engine, EngineConfig, EngineError, KeyringError, NetworkHealth, Portfolio};
use bahaa_market::{MarketConfig, Status};
use bahaa_vault::KdfParams;
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::watch;
use tokio_tungstenite::tungstenite::Message;
use url::Url;

const HARDHAT: &str = "test test test test test test test test test test test junk";
const USDT: &str = "0x55d398326f99059ff775485246999027b3197955";

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        let mut random = [0u8; 8];
        getrandom::fill(&mut random).unwrap();
        let name = random.iter().fold(String::new(), |mut out, byte| {
            let _ = write!(out, "{byte:02x}");
            out
        });
        Self(std::env::temp_dir().join(format!("bahaa-portfolio-{name}")))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

async fn node(calls: Arc<Mutex<usize>>, delay: Duration) -> Url {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
    tokio::spawn(async move {
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            let calls = Arc::clone(&calls);
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
                *calls.lock().unwrap() += 1;
                if call["method"] != "eth_chainId" {
                    tokio::time::sleep(delay).await;
                }
                let result = match call["method"].as_str().unwrap() {
                    "eth_chainId" => json!("0x38"),
                    "eth_getBalance" => json!("0x14d1120d7b160000"),
                    "eth_call"
                        if call["params"][0]["to"]
                            .as_str()
                            .unwrap()
                            .to_ascii_lowercase()
                            == USDT =>
                    {
                        json!(format!("0x{:064x}", 5_000_000_000_000_000_000u128))
                    }
                    "eth_call" => json!(format!("0x{:064x}", 0)),
                    _ => Value::Null,
                };
                let text = json!({"jsonrpc": "2.0", "id": 1, "result": result}).to_string();
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

fn endpoint(url: Url) -> Endpoint {
    Endpoint {
        protocol: Protocol::JsonRpc,
        url,
        features: vec![Feature::Logs],
        min_gap_ms: 0,
    }
}

fn registry(bsc: Url) -> Registry {
    let builtin = Registry::builtin().unwrap();
    let pick = |id: &str| -> Network { builtin.get(&id.parse().unwrap()).unwrap().clone() };
    let mut bsc_network = pick("bsc");
    bsc_network.endpoints = vec![endpoint(bsc)];
    let mut ethereum = pick("ethereum");
    ethereum.endpoints = vec![endpoint(Url::parse("http://127.0.0.1:9/").unwrap())];
    Registry::with_networks(vec![bsc_network, ethereum]).unwrap()
}

fn config(scratch: &Scratch, registry: Registry, market: Option<MarketConfig>) -> EngineConfig {
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
    }
}

async fn until<T: Clone>(watcher: &mut watch::Receiver<T>, done: impl Fn(&T) -> bool) -> T {
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

fn amount(portfolio: &Portfolio, symbol: &str) -> String {
    portfolio
        .holdings
        .iter()
        .find(|holding| holding.symbol == symbol && holding.asset.network().as_str() == "bsc")
        .map(|holding| holding.amount.to_string())
        .unwrap()
}

#[tokio::test]
async fn unlocking_shows_balances_from_every_reachable_network() {
    let scratch = Scratch::new();
    let calls = Arc::new(Mutex::new(0));
    let engine = Engine::start(config(
        &scratch,
        registry(node(Arc::clone(&calls), Duration::ZERO).await),
        None,
    ))
    .unwrap();
    let mut portfolio = engine.portfolio();
    engine.create(SecretString::from("pw")).await.unwrap();
    engine
        .import(SecretString::from(HARDHAT), SecretString::from(""))
        .await
        .unwrap();
    let accounts = engine.accounts().await.unwrap();
    assert_eq!(accounts.len(), 1);
    assert_eq!(
        accounts[0].address.to_string(),
        "0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266"
    );
    let filled = until(&mut portfolio, |p| {
        p.networks.len() == 2 && !p.holdings.is_empty()
    })
    .await;
    assert_eq!(amount(&filled, "BNB"), "1.5");
    assert_eq!(amount(&filled, "USDT"), "5");
    assert_eq!(amount(&filled, "USDC"), "0");
    let health: Vec<(String, bool)> = filled
        .networks
        .iter()
        .map(|n| (n.network.to_string(), n.problem.is_some()))
        .collect();
    assert_eq!(
        health,
        [("bsc".to_owned(), false), ("ethereum".to_owned(), true)]
    );
    assert_eq!(filled.total, None);
    let before = *calls.lock().unwrap();
    engine.refresh();
    tokio::time::timeout(Duration::from_secs(5), async {
        while *calls.lock().unwrap() <= before {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn locking_hides_accounts_and_balances() {
    let scratch = Scratch::new();
    let engine = Engine::start(config(
        &scratch,
        registry(node(Arc::default(), Duration::ZERO).await),
        None,
    ))
    .unwrap();
    let mut portfolio = engine.portfolio();
    engine.create(SecretString::from("pw")).await.unwrap();
    engine
        .import(SecretString::from(HARDHAT), SecretString::from(""))
        .await
        .unwrap();
    until(&mut portfolio, |p| !p.holdings.is_empty()).await;
    engine.lock().await.unwrap();
    let cleared = until(&mut portfolio, |p| p.holdings.is_empty()).await;
    assert_eq!(cleared.networks, Vec::<NetworkHealth>::new());
    assert_eq!(
        engine.accounts().await.unwrap_err(),
        EngineError::Keyring(KeyringError::Locked)
    );
}

#[tokio::test]
async fn prices_are_asked_only_for_held_coins_in_the_chosen_fiat() {
    let scratch = Scratch::new();
    let stream = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let stream_url = Url::parse(&format!("ws://{}/", stream.local_addr().unwrap())).unwrap();
    let seen: Arc<Mutex<Vec<Value>>> = Arc::default();
    let record = Arc::clone(&seen);
    tokio::spawn(async move {
        let (socket, _) = stream.accept().await.unwrap();
        let mut socket = tokio_tungstenite::accept_async(socket).await.unwrap();
        while let Some(Ok(message)) = socket.next().await {
            if let Message::Text(text) = message {
                let value: Value = serde_json::from_str(&text).unwrap();
                if value["type"] == "subscribe" {
                    let reply = json!({"type": "snapshot", "id": "wallet", "result": [
                        {"category": "crypto", "symbol": "BNB", "prices": {"USDT": 700, "IRT": 81_000_000}, "updated_at_unix": 1},
                        {"category": "crypto", "symbol": "USDT", "prices": {"USDT": 1, "IRT": 115_000}, "updated_at_unix": 1}
                    ]});
                    let _ = socket.send(Message::text(reply.to_string())).await;
                }
                record.lock().unwrap().push(value);
            }
        }
    });
    let market = MarketConfig {
        api: Url::parse("http://127.0.0.1:9/").unwrap(),
        stream: stream_url,
        poll_every: Duration::from_secs(60),
        backoff_start: Duration::from_millis(50),
        backoff_max: Duration::from_millis(100),
        silence_limit: Duration::from_secs(5),
        connect_timeout: Duration::from_secs(2),
    };
    let engine = Engine::start(config(
        &scratch,
        registry(node(Arc::default(), Duration::ZERO).await),
        Some(market),
    ))
    .unwrap();
    let mut portfolio = engine.portfolio();
    engine.create(SecretString::from("pw")).await.unwrap();
    engine
        .import(SecretString::from(HARDHAT), SecretString::from(""))
        .await
        .unwrap();
    let valued = until(&mut portfolio, |p| {
        p.total.is_some() && p.prices == Status::Live
    })
    .await;
    assert_eq!(valued.total.unwrap().fixed(), "1055.00");
    let first = seen
        .lock()
        .unwrap()
        .iter()
        .find(|v| v["type"] == "subscribe")
        .cloned()
        .unwrap();
    assert_eq!(
        first["condition"]["conditions"][0]["conditions"][1]["right"],
        json!(["BNB", "USDT"])
    );
    assert_eq!(first["bases"], json!(["USDT"]));
    engine
        .set_settings(Settings {
            fiat: Fiat::Irt,
            ..Settings::default()
        })
        .await
        .unwrap();
    let toman = until(&mut portfolio, |p| p.fiat == Fiat::Irt && p.total.is_some()).await;
    assert_eq!(toman.total.unwrap().fixed(), "122075000");
    let mut prices = engine.prices();
    engine.lock().await.unwrap();
    until(&mut prices, |snapshot| snapshot.status == Status::Idle).await;
}

async fn wait_for(calls: &Arc<Mutex<usize>>, at_least: usize) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while *calls.lock().unwrap() < at_least {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_wallet_added_during_a_refresh_shows_up_right_away() {
    let scratch = Scratch::new();
    let calls = Arc::new(Mutex::new(0));
    let slow = node(Arc::clone(&calls), Duration::from_millis(150)).await;
    let engine = Engine::start(config(&scratch, registry(slow), None)).unwrap();
    let mut portfolio = engine.portfolio();
    engine.create(SecretString::from("pw")).await.unwrap();
    engine
        .import(SecretString::from(HARDHAT), SecretString::from(""))
        .await
        .unwrap();
    wait_for(&calls, 2).await;
    engine
        .generate(WordCount::Twelve, SecretString::from(""))
        .await
        .unwrap();
    let both = until(&mut portfolio, |p| {
        let mut addresses: Vec<_> = p.holdings.iter().map(|h| h.address).collect();
        addresses.dedup();
        addresses.len() == 2
    })
    .await;
    assert_eq!(both.networks.len(), 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn locking_stops_a_refresh_in_flight() {
    let scratch = Scratch::new();
    let calls = Arc::new(Mutex::new(0));
    let slow = node(Arc::clone(&calls), Duration::from_millis(300)).await;
    let engine = Engine::start(config(&scratch, registry(slow), None)).unwrap();
    engine.create(SecretString::from("pw")).await.unwrap();
    engine
        .import(SecretString::from(HARDHAT), SecretString::from(""))
        .await
        .unwrap();
    wait_for(&calls, 2).await;
    engine.lock().await.unwrap();
    let after_lock = *calls.lock().unwrap();
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert_eq!(*calls.lock().unwrap(), after_lock);
}
