#![allow(clippy::unwrap_used)]

mod common;

use std::fmt::Write;
use std::time::Duration;

use bahaa_chain::ChainError;
use bahaa_config::{Coin, Registry};
use bahaa_crypto::SecretString;
use bahaa_engine::{CustomToken, Engine, EngineError, Event, KeyringError, StoreError};
use bahaa_types::{Address, AssetId, ChainFamily};
use common::{HARDHAT, Scratch, TRON_0, USDT, config, until};
use serde_json::{Value, json};
use tokio::sync::broadcast;
use url::Url;

const CAKE: &str = "0x0e09fabb73bd3ade0a17ecc321fd13a19e81ce82";
const SPOOF: &str = "0x1111111111111111111111111111111111111111";
const SPACED: &str = "0x2222222222222222222222222222222222222222";

fn text(value: &str) -> String {
    let mut padded = value.as_bytes().to_vec();
    padded.resize(value.len().div_ceil(32) * 32, 0);
    let body = padded.iter().fold(String::new(), |mut out, byte| {
        let _ = write!(out, "{byte:02x}");
        out
    });
    format!("0x{:064x}{:064x}{body}", 32, value.len())
}

fn word(value: u128) -> String {
    format!("0x{value:064x}")
}

async fn bsc() -> Url {
    common::node(|call| {
        let reply = match call["method"].as_str().unwrap() {
            "eth_chainId" => json!("0x38"),
            "eth_getBalance" => json!("0x0"),
            "eth_call" => {
                let to = call["params"][0]["to"]
                    .as_str()
                    .unwrap()
                    .to_ascii_lowercase();
                let data = call["params"][0]["data"].as_str().unwrap();
                let details = match to.as_str() {
                    CAKE => Some(("Cake", "PancakeSwap\u{202e} Token", 18)),
                    SPOOF => Some(("usdt", "Tether USD", 6)),
                    SPACED => Some(("C A K E", "Spaced", 18)),
                    _ => None,
                };
                match (details, &data[..10]) {
                    (Some((_, _, decimals)), "0x313ce567") => json!(word(decimals)),
                    (Some((symbol, _, _)), "0x95d89b41") => json!(text(symbol)),
                    (Some((_, name, _)), "0x06fdde03") => json!(text(name)),
                    (Some(_), "0x70a08231") => json!(word(7_000_000_000_000_000_000)),
                    (None, "0x70a08231") if to == USDT => json!("0x"),
                    (None, "0x70a08231") => json!(word(0)),
                    _ => json!("0x"),
                }
            }
            _ => Value::Null,
        };
        (common::result(reply), Duration::ZERO)
    })
    .await
}

fn registry(bsc: Url) -> Registry {
    let dead = Url::parse("http://127.0.0.1:9/").unwrap();
    Registry::with_networks(vec![
        common::network("bsc", bsc),
        common::tron_network(dead),
    ])
    .unwrap()
}

fn asset(contract: &str) -> AssetId {
    format!("bsc/{contract}").parse().unwrap()
}

fn cake() -> CustomToken {
    CustomToken {
        asset: asset(CAKE),
        coin: Coin {
            symbol: "Cake".to_owned(),
            name: "PancakeSwap Token".to_owned(),
            decimals: 18,
            price: None,
            color: None,
        },
    }
}

async fn changed(events: &mut broadcast::Receiver<Event>) -> Vec<CustomToken> {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Ok(Event::TokensChanged(tokens)) = events.recv().await {
                return tokens;
            }
        }
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn a_custom_token_is_read_from_its_contract_shown_and_kept() {
    let scratch = Scratch::new();
    let url = bsc().await;
    let engine = Engine::start(config(&scratch, registry(url.clone()), None)).unwrap();
    let mut events = engine.events();
    let mut portfolio = engine.portfolio();
    engine.create(SecretString::from("pw")).await.unwrap();
    engine
        .import(SecretString::from(HARDHAT), SecretString::from(""))
        .await
        .unwrap();
    assert_eq!(engine.add_token(asset(CAKE)).await.unwrap(), cake());
    assert_eq!(changed(&mut events).await, [cake()]);
    let shown = until(&mut portfolio, |p| {
        p.holdings
            .iter()
            .any(|holding| holding.asset == asset(CAKE))
    })
    .await;
    let holding = shown
        .holdings
        .iter()
        .find(|holding| holding.asset == asset(CAKE))
        .unwrap();
    assert_eq!(
        (holding.symbol.as_str(), holding.amount.to_string()),
        ("Cake", "7".to_owned())
    );
    let bsc = shown
        .networks
        .iter()
        .find(|health| health.network.as_str() == "bsc")
        .unwrap();
    assert_eq!(
        bsc.problem.as_deref(),
        Some("this address is not a token contract")
    );
    assert_eq!(engine.tokens().await.unwrap(), [cake()]);
    engine.shutdown().await.unwrap();
    let engine = Engine::start(config(&scratch, registry(url), None)).unwrap();
    let mut events = engine.events();
    let mut portfolio = engine.portfolio();
    assert_eq!(
        engine.tokens().await,
        Err(EngineError::Keyring(KeyringError::Locked))
    );
    engine.unlock(SecretString::from("pw")).await.unwrap();
    assert_eq!(engine.tokens().await.unwrap(), [cake()]);
    until(&mut portfolio, |p| {
        p.holdings
            .iter()
            .any(|holding| holding.asset == asset(CAKE))
    })
    .await;
    assert_eq!(engine.remove_token(asset(CAKE)).await, Ok(true));
    assert_eq!(changed(&mut events).await, []);
    assert!(
        !engine
            .portfolio()
            .borrow()
            .holdings
            .iter()
            .any(|holding| holding.asset == asset(CAKE))
    );
    assert_eq!(engine.remove_token(asset(CAKE)).await, Ok(false));
}

#[tokio::test]
async fn tokens_that_cannot_be_trusted_or_shown_are_refused() {
    let scratch = Scratch::new();
    let engine = Engine::start(config(&scratch, registry(bsc().await), None)).unwrap();
    engine.create(SecretString::from("pw")).await.unwrap();
    let invalid = |reason: &'static str| Err(EngineError::Chain(ChainError::Invalid(reason)));
    assert_eq!(
        engine.add_token(asset(USDT)).await,
        invalid("this token is already listed")
    );
    assert_eq!(
        engine.add_token(asset(SPOOF)).await,
        invalid("a listed coin on this network already uses this symbol")
    );
    assert_eq!(
        engine.add_token(asset(common::HARDHAT_0)).await,
        invalid("this address is not a token contract")
    );
    assert!(matches!(
        engine.add_token(asset(SPACED)).await,
        Err(EngineError::Store(StoreError::Invalid(_)))
    ));
    assert_eq!(
        engine.add_token("bsc".parse().unwrap()).await,
        invalid("a network's own coin is always shown")
    );
    let elsewhere = AssetId::Token(
        "ethereum".parse().unwrap(),
        Address::parse(ChainFamily::Evm, CAKE).unwrap(),
    );
    assert_eq!(
        engine.add_token(elsewhere).await,
        Err(EngineError::UnknownNetwork)
    );
    let mixed = AssetId::Token(
        "bsc".parse().unwrap(),
        Address::parse(ChainFamily::Tron, TRON_0).unwrap(),
    );
    assert_eq!(
        engine.add_token(mixed).await,
        invalid("this address is for another kind of network")
    );
    assert_eq!(engine.tokens().await.unwrap(), []);
    engine.lock().await.unwrap();
    assert_eq!(
        engine.add_token(asset(CAKE)).await,
        Err(EngineError::Keyring(KeyringError::Locked))
    );
}
