#![allow(clippy::unwrap_used)]

mod common;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use bahaa_chain::ChainError;
use bahaa_config::{Endpoint, Network, Protocol, Registry};
use bahaa_crypto::SecretString;
use bahaa_engine::{Engine, EngineError, Event, Received, SendRequest};
use bahaa_types::{Amount, ChainFamily, NetworkId};
use common::{HARDHAT, HARDHAT_0, Scratch, config, until};
use serde_json::{Value, json};
use url::Url;

const MAINNET: &str = "17a3a92992aabea785a7a090985a265cd31f323d849da51239737e321fb05569";
const TESTNET: &str = "823f81f306ff02694f935cf5021548e3ce2b86b529812af6a12148879e95a128";
const TON_0: &str = "0:c73162b34cf19667210af3180d6fcfb88df1678cbbbe731f38642534cc6951e4";
const WALLET: &str = include_str!("fixtures/ton_wallet.json");

type Asked = Arc<Mutex<Vec<String>>>;

async fn tonapi(zero_state: &'static str, asked: &Asked) -> Url {
    let asked = Arc::clone(asked);
    common::rest_node(move |path, _| -> Value {
        asked.lock().unwrap().push(path.to_owned());
        if path == "/liteserver/get_masterchain_info" {
            json!({"init": {"root_hash": zero_state}})
        } else if let Some(answer) = recorded(path) {
            answer
        } else {
            json!({"balance": 2_500_000_000_u64, "status": "active"})
        }
    })
    .await
}

fn recorded(path: &str) -> Option<Value> {
    let fixture: Value = serde_json::from_str(WALLET).unwrap();
    if path == format!("/liteserver/get_account_state/{TON_0}") {
        return Some(fixture["state"].clone());
    }
    let listed = format!("/liteserver/get_transactions/{TON_0}?count=16&lt=");
    let (lt, _) = path.strip_prefix(&listed)?.split_once("&hash=")?;
    Some(fixture["transactions"][lt].clone()).filter(|answer| !answer.is_null())
}

fn ton(id: &str, url: Url) -> Network {
    let mut network = Registry::builtin()
        .unwrap()
        .get(&id.parse().unwrap())
        .unwrap()
        .clone();
    network.endpoints = vec![Endpoint {
        protocol: Protocol::TonApi,
        url,
        features: Vec::new(),
        min_gap_ms: 0,
    }];
    network.tokens.clear();
    network
}

#[tokio::test]
async fn ton_accounts_follow_each_networks_wallet_id_and_show_gram() {
    let scratch = Scratch::new();
    let (main_asked, test_asked) = (Asked::default(), Asked::default());
    let dead = Url::parse("http://127.0.0.1:9/").unwrap();
    let registry = Registry::with_networks(vec![
        common::network("bsc", dead.clone()),
        common::network("bsc-testnet", dead),
        ton("ton", tonapi(MAINNET, &main_asked).await),
        ton("ton-testnet", tonapi(TESTNET, &test_asked).await),
    ])
    .unwrap();
    let engine = Engine::start(config(&scratch, registry, None)).unwrap();
    let mut portfolio = engine.portfolio();
    engine.create(SecretString::from("pw")).await.unwrap();
    let wallet = engine
        .import(SecretString::from(HARDHAT), SecretString::from(""))
        .await
        .unwrap();
    let accounts = engine.accounts().await.unwrap();
    let shape: Vec<(ChainFamily, Option<String>)> = accounts
        .iter()
        .map(|account| {
            (
                account.family,
                account.network.as_ref().map(NetworkId::to_string),
            )
        })
        .collect();
    assert_eq!(
        shape,
        [
            (ChainFamily::Evm, None),
            (ChainFamily::Ton, Some("ton".to_owned())),
            (ChainFamily::Ton, Some("ton-testnet".to_owned())),
        ]
    );
    assert_eq!(accounts[0].address.to_string(), HARDHAT_0);
    let (main, test) = (accounts[1].address, accounts[2].address);
    assert_ne!(main, test);
    let filled = until(&mut portfolio, |p| {
        p.holdings.iter().any(|holding| holding.symbol == "GRAM")
    })
    .await;
    let gram = filled
        .holdings
        .iter()
        .find(|holding| holding.symbol == "GRAM")
        .unwrap();
    assert_eq!(
        (gram.address, gram.amount.to_string()),
        (main, "2.5".to_owned())
    );
    assert_eq!(gram.asset.network().as_str(), "ton");
    assert!(
        main_asked
            .lock()
            .unwrap()
            .contains(&format!("/accounts/{main}"))
    );
    assert!(
        !main_asked
            .lock()
            .unwrap()
            .contains(&format!("/accounts/{test}"))
    );
    assert_eq!(*test_asked.lock().unwrap(), Vec::<String>::new());
    let request = SendRequest {
        wallet,
        asset: "ton".parse().unwrap(),
        to: test,
        amount: Amount::parse("1", 9).unwrap(),
    };
    assert!(matches!(
        engine.prepare_send(request).await,
        Err(EngineError::Chain(ChainError::Network(_)))
    ));
    assert!(
        main_asked
            .lock()
            .unwrap()
            .contains(&format!("/liteserver/get_account_state/{main}"))
    );
}

fn received(id: &str, from: &str, lt: u64, time: u64) -> Received {
    Received {
        id: id.to_owned(),
        index: 0,
        network: "ton".parse().unwrap(),
        from: bahaa_types::Address::parse(ChainFamily::Ton, from).unwrap(),
        to: bahaa_types::Address::parse(ChainFamily::Ton, TON_0).unwrap(),
        asset: "ton".parse().unwrap(),
        amount: Amount::parse("0.0001", 9).unwrap(),
        block: lt,
        time,
    }
}

#[tokio::test]
async fn incoming_gram_joins_the_wallets_history_from_proven_state() {
    let scratch = Scratch::new();
    let asked = Asked::default();
    let registry =
        Registry::with_networks(vec![ton("ton", tonapi(MAINNET, &asked).await)]).unwrap();
    let engine = Engine::start(config(&scratch, registry, None)).unwrap();
    let mut events = engine.events();
    engine.create(SecretString::from("pw")).await.unwrap();
    engine
        .import(SecretString::from(HARDHAT), SecretString::from(""))
        .await
        .unwrap();
    let fresh: Vec<Received> = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Ok(Event::Received(entries)) = events.recv().await {
                return entries;
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(
        fresh,
        [
            received(
                "90098ab1a048a271603eef7b2b4bea387ed0303be5afe7fe0a0aa5a9f5402d34",
                "0:dd8f0a38d31cd40b9a7b9582f820ecd096ba7e884bd35c3c8782f8a3379bc4f8",
                88_210_222_000_007,
                1_783_275_994
            ),
            received(
                "e0e956c2bf8370c17da7fa93b366f526fe3a06c7bb999448fab1dbd16cff7d55",
                "0:a4a6b4d9847bf95cd37230ab1053825c734e6508d0fa8c23e33b18325c5f34f0",
                88_208_524_000_001,
                1_783_275_313
            ),
        ]
    );
    let incoming = engine.incoming().await.unwrap();
    assert_eq!(incoming.transfers, fresh);
    let coverage = &incoming.coverage[0];
    assert_eq!(
        (
            coverage.network.as_str(),
            coverage.address.to_string(),
            coverage.next,
            coverage.complete_from,
            coverage.complete_since
        ),
        (
            "ton",
            TON_0.to_owned(),
            88_210_222_000_008,
            88_206_230_000_005,
            1_783_274_391
        )
    );
}
