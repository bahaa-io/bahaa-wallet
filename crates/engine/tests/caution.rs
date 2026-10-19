#![allow(clippy::unwrap_used)]

mod common;

use std::time::Duration;

use bahaa_config::Registry;
use bahaa_crypto::SecretString;
use bahaa_engine::{Arrival, Caution, Engine, Event, SendRequest, Spend, WalletId};
use bahaa_market::{MarketConfig, Status};
use bahaa_types::{Address, Amount, ChainFamily};
use common::{HARDHAT, HARDHAT_0, HARDHAT_1, Scratch, USDT, config, result, until};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use sha3::{Digest, Keccak256};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message;
use url::Url;

const TRANSFER: &str = "0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef";
const GENESIS: u64 = 1_760_000_000;
const HEAD: u64 = 10_000;
const LIKE_OURS: &str = "0xf39f000000000000000000000000000000002266";
const LIKE_THEIRS: &str = "0x70990000000000000000000000000000000079c8";
const FRIEND: &str = "0x3C44CdDdB6a900fa2b585dd299e03d12FA4293BC";
const LIKE_FRIEND: &str = "0x3c440000000000000000000000000000000093bc";

fn number(value: &Value) -> u64 {
    u64::from_str_radix(value.as_str().unwrap().trim_start_matches("0x"), 16).unwrap()
}

fn padded(address: &str) -> String {
    format!(
        "0x{:0>64}",
        address.trim_start_matches("0x").to_ascii_lowercase()
    )
}

fn answer(call: &Value) -> Value {
    let params = &call["params"];
    let reply = match call["method"].as_str().unwrap() {
        "eth_chainId" => json!("0x38"),
        "eth_getBalance" => json!("0x8ac7230489e80000"),
        "eth_getTransactionCount" => json!("0x0"),
        "eth_feeHistory" => {
            json!({"oldestBlock": "0x1", "baseFeePerGas": ["0x3b9aca00", "0x3b9aca00"]})
        }
        "eth_maxPriorityFeePerGas" => json!("0x3b9aca00"),
        "eth_estimateGas" => json!("0x5208"),
        "eth_call" => json!(format!("0x{:064x}", 5_000_000_000_000_000_000u128)),
        "eth_blockNumber" => json!(format!("0x{HEAD:x}")),
        "eth_getBlockByNumber" => {
            json!({"timestamp": format!("0x{:x}", GENESIS + number(&params[0]))})
        }
        "eth_getLogs" => {
            let (from, to) = (
                number(&params[0]["fromBlock"]),
                number(&params[0]["toBlock"]),
            );
            json!(if (from..=to).contains(&9_000) {
                vec![json!({
                    "address": USDT,
                    "topics": [TRANSFER, padded(LIKE_OURS), padded(HARDHAT_0)],
                    "data": format!("0x{:064x}", 1),
                    "blockNumber": "0x2328",
                    "transactionHash": format!("0x{:064x}", 9_000),
                    "logIndex": "0x0",
                    "removed": false
                })]
            } else {
                Vec::new()
            })
        }
        "eth_sendRawTransaction" => {
            let raw = params[0].as_str().unwrap().trim_start_matches("0x");
            let bytes: Vec<u8> = (0..raw.len())
                .step_by(2)
                .map(|at| u8::from_str_radix(&raw[at..at + 2], 16).unwrap())
                .collect();
            json!(format!("0x{}", common::hex(&Keccak256::digest(bytes))))
        }
        _ => Value::Null,
    };
    result(reply)
}

async fn market() -> MarketConfig {
    let stream = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = Url::parse(&format!("ws://{}/", stream.local_addr().unwrap())).unwrap();
    tokio::spawn(async move {
        let (socket, _) = stream.accept().await.unwrap();
        let mut socket = tokio_tungstenite::accept_async(socket).await.unwrap();
        while let Some(Ok(message)) = socket.next().await {
            if let Message::Text(text) = message
                && serde_json::from_str::<Value>(&text).unwrap()["type"] == "subscribe"
            {
                let reply = json!({"type": "snapshot", "id": "wallet", "result": [
                    {"category": "crypto", "symbol": "BNB", "prices": {"USDT": 700}, "updated_at_unix": 1},
                    {"category": "crypto", "symbol": "USDT", "prices": {"USDT": 1}, "updated_at_unix": 1}
                ]});
                let _ = socket.send(Message::text(reply.to_string())).await;
            }
        }
    });
    MarketConfig {
        api: Url::parse("http://127.0.0.1:9/").unwrap(),
        stream: url,
        poll_every: Duration::from_secs(60),
        backoff_start: Duration::from_millis(50),
        backoff_max: Duration::from_millis(100),
        silence_limit: Duration::from_secs(5),
        connect_timeout: Duration::from_secs(2),
    }
}

fn bnb(wallet: WalletId, to: &str) -> SendRequest {
    SendRequest {
        account: 0,
        wallet,
        asset: "bsc".parse().unwrap(),
        to: Address::parse(ChainFamily::Evm, to).unwrap(),
        amount: Spend::Exact(Amount::parse("0.1", 18).unwrap()),
        memo: None,
    }
}

fn evm(text: &str) -> Address {
    Address::parse(ChainFamily::Evm, text).unwrap()
}

#[tokio::test]
async fn look_alikes_and_dust_are_flagged_where_they_arrive_and_where_they_are_sent() {
    let scratch = Scratch::new();
    let url = common::node(|call| (answer(call), Duration::ZERO)).await;
    let registry = Registry::with_networks(vec![common::network("bsc", url)]).unwrap();
    let engine = Engine::start(config(&scratch, registry, Some(market().await))).unwrap();
    let mut events = engine.events();
    let mut portfolio = engine.portfolio();
    engine.create(SecretString::from("pw")).await.unwrap();
    let wallet = engine
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
    assert_eq!(fresh[0].received.from, evm(LIKE_OURS));
    assert_eq!(fresh[0].cautions[0], Caution::LooksLike(evm(HARDHAT_0)));
    until(&mut portfolio, |p| {
        p.prices == Status::Live && p.total.is_some()
    })
    .await;
    assert_eq!(
        engine.incoming().await.unwrap().transfers[0].cautions,
        [Caution::LooksLike(evm(HARDHAT_0)), Caution::Dust]
    );
    let poisoned = engine.prepare_send(bnb(wallet, LIKE_OURS)).await.unwrap();
    assert_eq!(
        poisoned.cautions,
        [Caution::LooksLike(evm(HARDHAT_0)), Caution::DustSender]
    );
    let honest = engine.prepare_send(bnb(wallet, HARDHAT_1)).await.unwrap();
    assert_eq!(honest.cautions, []);
    assert_eq!(
        engine
            .prepare_send(bnb(wallet, LIKE_THEIRS))
            .await
            .unwrap()
            .cautions,
        []
    );
    engine
        .confirm_send(honest.id, SecretString::from("pw"))
        .await
        .unwrap();
    assert_eq!(
        engine
            .prepare_send(bnb(wallet, LIKE_THEIRS))
            .await
            .unwrap()
            .cautions,
        [Caution::LooksLike(evm(HARDHAT_1))]
    );
}

#[tokio::test]
async fn saved_contacts_count_as_known_addresses() {
    let scratch = Scratch::new();
    let url = common::node(|call| (answer(call), Duration::ZERO)).await;
    let registry = Registry::with_networks(vec![common::network("bsc", url)]).unwrap();
    let engine = Engine::start(config(&scratch, registry, None)).unwrap();
    engine.create(SecretString::from("pw")).await.unwrap();
    engine
        .save_contact(bahaa_store::Contact {
            network: "bsc".parse().unwrap(),
            address: evm(FRIEND),
            name: "Friend".to_owned(),
        })
        .await
        .unwrap();
    let wallet = engine
        .import(SecretString::from(HARDHAT), SecretString::from(""))
        .await
        .unwrap();
    let to_friend = engine.prepare_send(bnb(wallet, FRIEND)).await.unwrap();
    assert_eq!(to_friend.cautions, []);
    let to_impostor = engine.prepare_send(bnb(wallet, LIKE_FRIEND)).await.unwrap();
    assert_eq!(to_impostor.cautions, [Caution::LooksLike(evm(FRIEND))]);
}

#[tokio::test]
async fn saved_data_is_sealed_on_disk_and_closed_while_locked() {
    let scratch = Scratch::new();
    let url = common::node(|call| (answer(call), Duration::ZERO)).await;
    let registry = Registry::with_networks(vec![common::network("bsc", url)]).unwrap();
    let engine = Engine::start(config(&scratch, registry.clone(), None)).unwrap();
    let mut events = engine.events();
    engine.create(SecretString::from("pw")).await.unwrap();
    engine
        .import(SecretString::from(HARDHAT), SecretString::from(""))
        .await
        .unwrap();
    let friend = bahaa_store::Contact {
        network: "bsc".parse().unwrap(),
        address: evm(FRIEND),
        name: "Friend".to_owned(),
    };
    engine.save_contact(friend.clone()).await.unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        while !matches!(events.recv().await, Ok(Event::Received(_))) {}
    })
    .await
    .unwrap();
    let arrived = engine.incoming().await.unwrap().transfers;
    assert_eq!(arrived.len(), 1);
    engine.lock().await.unwrap();
    assert!(engine.contacts().await.is_err());
    assert!(engine.incoming().await.is_err());
    engine.unlock(SecretString::from("pw")).await.unwrap();
    assert_eq!(
        engine.contacts().await.unwrap(),
        std::slice::from_ref(&friend)
    );
    assert_eq!(engine.incoming().await.unwrap().transfers, arrived);
    engine.shutdown().await.unwrap();
    drop(engine);
    let raw = std::fs::read(scratch.0.join("data").join("wallet.redb")).unwrap();
    let text = String::from_utf8_lossy(&raw).to_lowercase();
    for private in ["friend", "3c44cddd", "f39fd6e5", "70997970", "f39f0000"] {
        assert!(!text.contains(private), "{private}");
    }
    let engine = Engine::start(config(&scratch, registry, None)).unwrap();
    engine.unlock(SecretString::from("pw")).await.unwrap();
    assert_eq!(engine.contacts().await.unwrap(), [friend]);
    assert_eq!(engine.incoming().await.unwrap().transfers, arrived);
}
