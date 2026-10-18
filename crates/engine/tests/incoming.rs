#![allow(clippy::unwrap_used)]

mod common;

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bahaa_config::Registry;
use bahaa_crypto::SecretString;
use bahaa_engine::{Arrival, Engine, EngineError, Event, IncomingHistory, KeyringError};
use common::{HARDHAT, HARDHAT_0, HARDHAT_1, Scratch, USDT, config, result};
use serde_json::{Value, json};

const TRANSFER: &str = "0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef";
const GENESIS: u64 = 1_760_000_000;

#[derive(Default)]
struct FakeChain {
    head: AtomicU64,
    asked: Mutex<Vec<(u64, u64)>>,
}

fn number(value: &Value) -> u64 {
    u64::from_str_radix(value.as_str().unwrap().trim_start_matches("0x"), 16).unwrap()
}

fn padded(address: &str) -> String {
    format!(
        "0x{:0>64}",
        address.trim_start_matches("0x").to_ascii_lowercase()
    )
}

fn usdt_to_us(block: u64) -> Value {
    json!({
        "address": USDT,
        "topics": [TRANSFER, padded(HARDHAT_1), padded(HARDHAT_0)],
        "data": format!("0x{:064x}", 7_500_000_000_000_000_000u128),
        "blockNumber": format!("0x{block:x}"),
        "transactionHash": format!("0x{:064x}", block),
        "logIndex": "0x0",
        "removed": false
    })
}

fn usdt_from_us(block: u64) -> Value {
    json!({
        "address": USDT,
        "topics": [TRANSFER, padded(HARDHAT_0), padded(HARDHAT_1)],
        "data": format!("0x{:064x}", 1_000_000_000_000_000_000u128),
        "blockNumber": format!("0x{block:x}"),
        "transactionHash": format!("0x{:064x}", block + 1),
        "logIndex": "0x1",
        "removed": false
    })
}

async fn bsc(chain: &Arc<FakeChain>) -> Registry {
    let chain = Arc::clone(chain);
    let url = common::node(move |call| {
        let params = &call["params"];
        let reply = match call["method"].as_str().unwrap() {
            "eth_chainId" => json!("0x38"),
            "eth_getBalance" => json!("0x0"),
            "eth_call" => json!(format!("0x{:064x}", 0)),
            "eth_blockNumber" => json!(format!("0x{:x}", chain.head.load(Ordering::SeqCst))),
            "eth_getBlockByNumber" => {
                json!({"timestamp": format!("0x{:x}", GENESIS + number(&params[0]))})
            }
            "eth_getLogs" if params[0]["topics"][1] == json!(padded(HARDHAT_0)) => {
                let (from, to) = (
                    number(&params[0]["fromBlock"]),
                    number(&params[0]["toBlock"]),
                );
                json!(if (from..=to).contains(&8_000) {
                    vec![usdt_from_us(8_000)]
                } else {
                    Vec::new()
                })
            }
            "eth_getLogs" => {
                let (from, to) = (
                    number(&params[0]["fromBlock"]),
                    number(&params[0]["toBlock"]),
                );
                chain.asked.lock().unwrap().push((from, to));
                json!(if (from..=to).contains(&9_000) {
                    vec![usdt_to_us(9_000)]
                } else {
                    Vec::new()
                })
            }
            _ => Value::Null,
        };
        (result(reply), Duration::ZERO)
    })
    .await;
    Registry::with_networks(vec![common::network("bsc", url)]).unwrap()
}

async fn asked_at_least(chain: &FakeChain, count: usize) -> Vec<(u64, u64)> {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let asked = chain.asked.lock().unwrap().clone();
            if asked.len() >= count {
                return asked;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap()
}

async fn incoming_until(
    engine: &Engine,
    done: impl Fn(&IncomingHistory) -> bool,
) -> IncomingHistory {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let incoming = engine.incoming().await.unwrap();
            if done(&incoming) {
                return incoming;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn incoming_tokens_are_found_kept_and_scanned_onward() {
    let scratch = Scratch::new();
    let chain = Arc::new(FakeChain::default());
    chain.head.store(10_000, Ordering::SeqCst);
    let engine = Engine::start(config(&scratch, bsc(&chain).await, None)).unwrap();
    let mut events = engine.events();
    engine.create(SecretString::from("pw")).await.unwrap();
    engine
        .import(SecretString::from(HARDHAT), SecretString::from(""))
        .await
        .unwrap();
    let fresh: Vec<Arrival> = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Ok(Event::Received(entries)) = events.recv().await {
                return entries;
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(fresh.len(), 1);
    assert_eq!(fresh[0].cautions, []);
    let entry = &fresh[0].received;
    assert_eq!(
        (entry.from.to_string(), entry.to.to_string()),
        (HARDHAT_1.to_owned(), HARDHAT_0.to_owned())
    );
    assert_eq!(entry.amount.to_string(), "7.5");
    assert_eq!(entry.asset, format!("bsc/{USDT}").parse().unwrap());
    assert_eq!((entry.block, entry.time), (9_000, GENESIS + 9_000));
    let incoming = engine.incoming().await.unwrap();
    assert_eq!(incoming.transfers, fresh);
    let elsewhere = engine.outgoing().await.unwrap();
    assert_eq!(elsewhere.len(), 1);
    assert_eq!(
        (
            elsewhere[0].from.to_string(),
            elsewhere[0].to.to_string(),
            elsewhere[0].amount.to_string(),
            elsewhere[0].block
        ),
        (
            HARDHAT_0.to_owned(),
            HARDHAT_1.to_owned(),
            "1".to_owned(),
            8_000
        )
    );
    let coverage = &incoming.coverage[0];
    assert_eq!(
        (
            coverage.complete_from,
            coverage.next,
            coverage.complete_since
        ),
        (5_989, 9_989, GENESIS + 5_989)
    );
    assert_eq!(
        asked_at_least(&chain, 2).await,
        [(5_989, 7_988), (7_989, 9_988)]
    );
    chain.head.store(10_100, Ordering::SeqCst);
    engine.refresh();
    assert_eq!(asked_at_least(&chain, 3).await[2], (9_989, 10_088));
    incoming_until(&engine, |incoming| incoming.coverage[0].next == 10_089).await;
    engine.lock().await.unwrap();
    assert_eq!(
        engine.incoming().await,
        Err(EngineError::Keyring(KeyringError::Locked))
    );
    engine.shutdown().await.unwrap();

    chain.head.store(100_000, Ordering::SeqCst);
    let engine = Engine::start(config(&scratch, bsc(&chain).await, None)).unwrap();
    engine.unlock(SecretString::from("pw")).await.unwrap();
    assert_eq!(asked_at_least(&chain, 4).await[3], (95_989, 97_988));
    let after_gap = incoming_until(&engine, |incoming| {
        incoming.coverage[0].complete_from == 95_989
    })
    .await;
    assert_eq!(after_gap.coverage[0].complete_since, GENESIS + 95_989);
    assert_eq!(after_gap.transfers, fresh);
}
