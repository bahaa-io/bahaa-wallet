#![allow(clippy::unwrap_used)]

mod common;

use std::fmt::Write;
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

fn abi_string(text: &str) -> String {
    let mut padded = text.as_bytes().to_vec();
    padded.resize(text.len().div_ceil(32) * 32, 0);
    let body = padded.iter().fold(String::new(), |mut out, byte| {
        let _ = write!(out, "{byte:02x}");
        out
    });
    format!("0x{:064x}{:064x}{body}", 32, text.len())
}

#[tokio::test]
async fn a_custom_token_reports_its_own_details() {
    let url = node(Log::default(), |method, params| match method {
        "eth_chainId" => result(&json!("0x38")),
        "eth_call" => {
            let to = params[0]["to"].as_str().unwrap().to_ascii_lowercase();
            if to != CUSTOM.to_ascii_lowercase() {
                return result(&json!("0x"));
            }
            match params[0]["data"].as_str().unwrap() {
                "0x313ce567" => result(&json!(word(18))),
                "0x95d89b41" => result(&json!(abi_string("Cake"))),
                "0x06fdde03" => result(&json!(abi_string("PancakeSwap Token"))),
                _ => result(&json!("0x")),
            }
        }
        _ => result(&Value::Null),
    })
    .await;
    let chain = EvmChain::new(bsc_on(&[url])).unwrap();
    let custom = Address::parse(ChainFamily::Evm, CUSTOM).unwrap();
    let coin = chain.token(&custom).await.unwrap();
    assert_eq!(
        (
            coin.symbol.as_str(),
            coin.name.as_str(),
            coin.decimals,
            coin.price
        ),
        ("Cake", "PancakeSwap Token", 18, None)
    );
    assert_eq!(
        chain.token(&owner()).await,
        Err(ChainError::Invalid("this address is not a token contract"))
    );
    assert!(matches!(
        chain.token(&Address::Tron([7; 20])).await,
        Err(ChainError::Invalid(_))
    ));
}

#[tokio::test]
async fn known_decimals_are_used_without_asking_the_contract() {
    let url = node(Log::default(), |method, params| match method {
        "eth_chainId" => result(&json!("0x38")),
        "eth_call" if params[0]["data"] == "0x313ce567" => result(&json!("0x")),
        "eth_call" => result(&json!(word(1_234_567))),
        _ => result(&Value::Null),
    })
    .await;
    let chain = EvmChain::new(bsc_on(&[url])).unwrap();
    let custom = format!("bsc/{CUSTOM}");
    assert!(chain.balances(&owner(), &assets(&[&custom])).await.is_err());
    chain.know(&Address::parse(ChainFamily::Evm, CUSTOM).unwrap(), 9);
    let balances = chain.balances(&owner(), &assets(&[&custom])).await.unwrap();
    assert_eq!(balances[0].amount.to_string(), "0.001234567");
}

#[tokio::test]
#[ignore = "talks to public BSC endpoints"]
async fn live_bsc_token_details() {
    let registry = Registry::builtin().unwrap();
    let chain = EvmChain::new(registry.get(&"bsc".parse().unwrap()).unwrap().clone()).unwrap();
    let cake = Address::parse(ChainFamily::Evm, CUSTOM).unwrap();
    let coin = chain.token(&cake).await.unwrap();
    assert_eq!(
        (coin.symbol.as_str(), coin.name.as_str(), coin.decimals),
        ("Cake", "PancakeSwap Token", 18)
    );
    let usdt = Address::parse(ChainFamily::Evm, USDT).unwrap();
    let coin = chain.token(&usdt).await.unwrap();
    assert_eq!((coin.symbol.as_str(), coin.decimals), ("USDT", 18));
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
