#![allow(clippy::unwrap_used)]

use std::sync::{Arc, Mutex};

use bahaa_chain::{Chain, ChainError};
use bahaa_config::{Endpoint, Feature, Network, Protocol, Registry};
use bahaa_evm::EvmChain;
use bahaa_types::{Address, AssetId, ChainFamily};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use url::Url;

type Log = Arc<Mutex<Vec<String>>>;

const OWNER: &str = "0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266";
const USDT: &str = "0x55d398326f99059fF775485246999027B3197955";
const CUSTOM: &str = "0x0E09FaBB73Bd3Ade0a17ECC321fD13a19e81cE82";

fn word(value: u128) -> String {
    format!("0x{value:064x}")
}

async fn node(
    log: Log,
    reply: impl Fn(&str, &Value) -> (u16, Value) + Send + Sync + 'static,
) -> Url {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
    let reply = Arc::new(reply);
    tokio::spawn(async move {
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            let log = Arc::clone(&log);
            let reply = Arc::clone(&reply);
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
                let method = call["method"].as_str().unwrap().to_owned();
                log.lock().unwrap().push(method.clone());
                let (status, answer) = reply(&method, &call["params"]);
                let text = answer.to_string();
                let response = format!(
                    "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{text}",
                    text.len()
                );
                let _ = socket.write_all(response.as_bytes()).await;
                let _ = socket.shutdown().await;
            });
        }
    });
    url
}

fn result(value: &Value) -> (u16, Value) {
    (200, json!({"jsonrpc": "2.0", "id": 1, "result": value}))
}

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

fn bsc_on(urls: &[Url]) -> Network {
    let registry = Registry::builtin().unwrap();
    let mut network = registry.get(&"bsc".parse().unwrap()).unwrap().clone();
    network.endpoints = urls
        .iter()
        .map(|url| Endpoint {
            protocol: Protocol::JsonRpc,
            url: url.clone(),
            features: vec![Feature::Logs],
            min_gap_ms: 0,
        })
        .collect();
    network
}

fn owner() -> Address {
    Address::parse(ChainFamily::Evm, OWNER).unwrap()
}

fn assets(list: &[&str]) -> Vec<AssetId> {
    list.iter().map(|asset| asset.parse().unwrap()).collect()
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
