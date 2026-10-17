#![allow(clippy::unwrap_used)]

mod common;

use std::sync::Arc;

use bahaa_chain::{Chain, ChainError};
use bahaa_config::Registry;
use bahaa_evm::EvmChain;
use bahaa_types::{Address, ChainFamily};
use common::{CUSTOM, Log, USDT, assets, bsc_on, node, owner, result, word};
use serde_json::{Value, json};

fn healthy(chain: &'static str) -> impl Fn(&str, &Value) -> (u16, Value) + Send + Sync + 'static {
    move |method, params| match method {
        "eth_chainId" => result(&json!(chain)),
        "eth_getBalance" => result(&json!("0xde0b6b3a7640000")),
        "eth_call" => {
            let call = &params[0];
            let data = call["data"].as_str().unwrap();
            let to = call["to"].as_str().unwrap().to_ascii_lowercase();
            if data == "0x313ce567" {
                return result(&json!(word(6)));
            }
            if to == USDT.to_ascii_lowercase() {
                result(&json!(word(5_000_000_000_000_000_000)))
            } else {
                result(&json!(word(1_234_567)))
            }
        }
        _ => (
            200,
            json!({"jsonrpc": "2.0", "id": 1, "error": {"code": -32601, "message": "no such method"}}),
        ),
    }
}

#[tokio::test]
async fn reads_native_known_and_custom_token_balances() {
    let log = Log::default();
    let url = node(Arc::clone(&log), healthy("0x38")).await;
    let chain = EvmChain::new(bsc_on(&[url])).unwrap();
    let wanted = assets(&["bsc", &format!("bsc/{USDT}"), &format!("bsc/{CUSTOM}")]);
    let balances = chain.balances(&owner(), &wanted).await.unwrap();
    let shown: Vec<String> = balances
        .iter()
        .map(|balance| balance.amount.to_string())
        .collect();
    assert_eq!(shown, ["1", "5", "1.234567"]);
    assert_eq!(balances[1].amount.decimals(), 18);
    assert_eq!(balances[2].amount.decimals(), 6);
    let calls = log.lock().unwrap().clone();
    assert_eq!(
        calls
            .iter()
            .filter(|method| *method == "eth_chainId")
            .count(),
        1
    );
    chain.balances(&owner(), &wanted[2..]).await.unwrap();
    assert_eq!(log.lock().unwrap().len(), calls.len() + 1);
}

#[tokio::test]
async fn skips_an_endpoint_that_answers_for_another_chain() {
    let wrong_log = Log::default();
    let right_log = Log::default();
    let wrong = node(Arc::clone(&wrong_log), healthy("0x1")).await;
    let right = node(Arc::clone(&right_log), healthy("0x38")).await;
    let chain = EvmChain::new(bsc_on(&[wrong, right])).unwrap();
    let balances = chain.balances(&owner(), &assets(&["bsc"])).await.unwrap();
    assert_eq!(balances[0].amount.to_string(), "1");
    assert_eq!(*wrong_log.lock().unwrap(), ["eth_chainId"]);
    assert_eq!(
        *right_log.lock().unwrap(),
        ["eth_chainId", "eth_getBalance"]
    );
}

#[tokio::test]
async fn moves_on_when_an_endpoint_is_rate_limited() {
    let busy = node(Log::default(), |_, _| (429, json!({}))).await;
    let fine = node(Log::default(), healthy("0x38")).await;
    let chain = EvmChain::new(bsc_on(&[busy, fine])).unwrap();
    assert!(chain.balances(&owner(), &assets(&["bsc"])).await.is_ok());
}

#[tokio::test]
async fn a_reverting_call_is_reported_without_trying_elsewhere() {
    let second_log = Log::default();
    let reverting = node(Log::default(), |method, params| {
        if method == "eth_call" {
            return (200, json!({"jsonrpc": "2.0", "id": 1, "error": {"code": 3, "message": "execution reverted"}}));
        }
        healthy("0x38")(method, params)
    })
    .await;
    let second = node(Arc::clone(&second_log), healthy("0x38")).await;
    let chain = EvmChain::new(bsc_on(&[reverting, second])).unwrap();
    let err = chain
        .balances(&owner(), &assets(&[&format!("bsc/{USDT}")]))
        .await
        .unwrap_err();
    assert_eq!(err, ChainError::Rejected("execution reverted".to_owned()));
    assert!(second_log.lock().unwrap().is_empty());
}

#[tokio::test]
async fn an_address_without_code_is_not_a_token() {
    let empty = node(Log::default(), |method, params| {
        if method == "eth_call" {
            return result(&json!("0x"));
        }
        healthy("0x38")(method, params)
    })
    .await;
    let chain = EvmChain::new(bsc_on(&[empty])).unwrap();
    let err = chain
        .balances(&owner(), &assets(&[&format!("bsc/{CUSTOM}")]))
        .await
        .unwrap_err();
    assert!(matches!(err, ChainError::Invalid(_)));
}

#[tokio::test]
async fn refuses_assets_and_addresses_of_other_networks() {
    let url = node(Log::default(), healthy("0x38")).await;
    let chain = EvmChain::new(bsc_on(&[url])).unwrap();
    assert!(matches!(
        chain.balances(&owner(), &assets(&["ethereum"])).await,
        Err(ChainError::Invalid(_))
    ));
    let tron = Address::parse(ChainFamily::Tron, "TR7NHqjeKQxGTCi8q8ZY4pL8otSzgjLj6t").unwrap();
    assert!(matches!(
        chain.balances(&tron, &assets(&["bsc"])).await,
        Err(ChainError::Invalid(_))
    ));
}

#[tokio::test]
#[ignore = "talks to public BSC endpoints"]
async fn live_bsc_balances() {
    let registry = Registry::builtin().unwrap();
    let chain = EvmChain::new(registry.get(&"bsc".parse().unwrap()).unwrap().clone()).unwrap();
    let dead = Address::parse(
        ChainFamily::Evm,
        "0x000000000000000000000000000000000000dEaD",
    )
    .unwrap();
    let balances = chain
        .balances(&dead, &assets(&["bsc", &format!("bsc/{USDT}")]))
        .await
        .unwrap();
    assert!(!balances[0].amount.is_zero());
    assert_eq!(balances[1].amount.decimals(), 18);
}
