#![allow(clippy::unwrap_used)]

mod common;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use bahaa_chain::ChainError;
use bahaa_config::{Fiat, Registry, Settings};
use bahaa_crypto::{SecretString, WordCount};
use bahaa_engine::{Engine, EngineError, KeyringError, NetworkHealth, Portfolio, SendRequest};
use bahaa_market::{MarketConfig, Status};
use bahaa_types::{Address, Amount, ChainFamily};
use common::{HARDHAT, HARDHAT_0, Scratch, TRON_0, TRON_0_HEX, TRON_GENESIS, USDT, config, until};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message;
use url::Url;

async fn node(calls: Arc<Mutex<usize>>, delay: Duration) -> Url {
    common::node(move |call| {
        *calls.lock().unwrap() += 1;
        let method = call["method"].as_str().unwrap();
        let reply = match method {
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
        let wait = if method == "eth_chainId" {
            Duration::ZERO
        } else {
            delay
        };
        (common::result(reply), wait)
    })
    .await
}

fn registry(bsc: Url) -> Registry {
    let dead = Url::parse("http://127.0.0.1:9/").unwrap();
    Registry::with_networks(vec![
        common::network("bsc", bsc),
        common::network("ethereum", dead),
    ])
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

async fn tron_node() -> Url {
    common::node(|call| {
        let reply = if call["num"] == 0 {
            json!({"blockID": TRON_GENESIS})
        } else if call.get("function_selector").is_some() && call["owner_address"] == TRON_0_HEX {
            json!({
                "result": {"result": true},
                "constant_result": [format!("{:064x}", 4_250_000)],
                "transaction": {"ret": [{}]}
            })
        } else if call["address"] == TRON_0_HEX {
            json!({"address": TRON_0_HEX, "balance": 12_000_000})
        } else {
            json!({})
        };
        (reply, Duration::ZERO)
    })
    .await
}

#[tokio::test]
async fn tron_holdings_join_the_portfolio_under_the_wallets_tron_account() {
    let scratch = Scratch::new();
    let registry = Registry::with_networks(vec![
        common::network("bsc", node(Arc::default(), Duration::ZERO).await),
        common::tron_network(tron_node().await),
    ])
    .unwrap();
    let engine = Engine::start(config(&scratch, registry, None)).unwrap();
    let mut portfolio = engine.portfolio();
    engine.create(SecretString::from("pw")).await.unwrap();
    let wallet = engine
        .import(SecretString::from(HARDHAT), SecretString::from(""))
        .await
        .unwrap();
    let accounts: Vec<(ChainFamily, String)> = engine
        .accounts()
        .await
        .unwrap()
        .iter()
        .map(|account| (account.family, account.address.to_string()))
        .collect();
    assert_eq!(
        accounts,
        [
            (ChainFamily::Evm, HARDHAT_0.to_owned()),
            (ChainFamily::Tron, TRON_0.to_owned())
        ]
    );
    let filled = until(&mut portfolio, |p| {
        p.networks.len() == 2 && p.holdings.iter().any(|holding| holding.symbol == "TRX")
    })
    .await;
    let mut held: Vec<(String, String, String, String)> = filled
        .holdings
        .iter()
        .map(|holding| {
            (
                holding.asset.network().to_string(),
                holding.symbol.clone(),
                holding.amount.to_string(),
                holding.address.to_string(),
            )
        })
        .collect();
    held.retain(|(network, ..)| network == "tron");
    held.sort();
    assert_eq!(
        held,
        [
            (
                "tron".to_owned(),
                "TRX".to_owned(),
                "12".to_owned(),
                TRON_0.to_owned()
            ),
            (
                "tron".to_owned(),
                "USDT".to_owned(),
                "4.25".to_owned(),
                TRON_0.to_owned()
            ),
        ]
    );
    assert!(
        filled
            .holdings
            .iter()
            .any(|holding| { holding.symbol == "BNB" && holding.address.to_string() == HARDHAT_0 })
    );
    assert!(
        filled
            .networks
            .iter()
            .all(|network| network.problem.is_none())
    );
    let request = SendRequest {
        wallet,
        asset: "tron".parse().unwrap(),
        to: Address::parse(ChainFamily::Tron, TRON_0).unwrap(),
        amount: Amount::parse("1", 6).unwrap(),
    };
    assert_eq!(
        engine.prepare_send(request).await,
        Err(EngineError::Chain(ChainError::Unsupported("sending yet")))
    );
}
