#![allow(clippy::unwrap_used)]

mod common;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bahaa_config::Registry;
use bahaa_crypto::SecretString;
use bahaa_engine::swap::{Charged, CostKind, Method, Providers, SwapStatus};
use bahaa_engine::{Engine, Event, SwapRequest};
use bahaa_testkit::{Reply, serve};
use bahaa_types::{Address, Amount, ChainFamily};
use common::{HARDHAT, HARDHAT_0, Scratch, config, result, unhex};
use serde_json::{Value, json};
use sha3::{Digest, Keccak256};
use url::Url;

const DEPOSIT: &str = "0x88346a0010f9c3914b9e6d507a7200bdcdede3a5";
const REQUEST: &str = "0x1793720631f6df061cade369e4f053582da6700bb930246d73dc27ffad60ddb9";

fn node(sent: &Arc<Mutex<Vec<String>>>, call: &Value) -> Value {
    let first = call["params"][0].as_str().unwrap_or_default().to_owned();
    let mut sent = sent.lock().unwrap();
    let known = |hash: &str| {
        sent.iter().any(|raw| {
            format!(
                "0x{}",
                common::hex(&Keccak256::digest(unhex(raw.trim_start_matches("0x"))))
            ) == hash
        })
    };
    let reply = match call["method"].as_str().unwrap() {
        "eth_chainId" => json!("0x38"),
        "eth_getBalance" => json!("0x8ac7230489e80000"),
        "eth_getTransactionCount" => json!("0x0"),
        "eth_feeHistory" => {
            json!({"oldestBlock": "0x1", "baseFeePerGas": ["0x3b9aca00", "0x3b9aca00"]})
        }
        "eth_maxPriorityFeePerGas" => json!("0x3b9aca00"),
        "eth_estimateGas" => json!("0x5208"),
        "eth_getCode" => json!("0x"),
        "eth_call" => json!(format!("0x{:064x}", 0)),
        "eth_sendRawTransaction" => {
            sent.push(first.clone());
            json!(format!(
                "0x{}",
                common::hex(&Keccak256::digest(unhex(first.trim_start_matches("0x"))))
            ))
        }
        "eth_getTransactionReceipt" if known(&first) => json!({
            "status": "0x1",
            "blockNumber": "0x10",
            "gasUsed": "0x5208",
            "effectiveGasPrice": "0x77359400"
        }),
        "eth_getTransactionByHash" if known(&first) => json!({"hash": first}),
        _ => Value::Null,
    };
    result(reply)
}

fn quote(body: &Value) -> Value {
    let amount = body["amount"].as_str().unwrap().to_owned();
    json!({
        "steps": [{
            "id": "deposit",
            "kind": "transaction",
            "requestId": REQUEST,
            "depositAddress": DEPOSIT,
            "items": [{"data": {
                "from": HARDHAT_0,
                "to": DEPOSIT,
                "data": "0x",
                "value": amount,
                "chainId": 56
            }}]
        }],
        "details": {
            "currencyIn": {"amount": amount},
            "currencyOut": {"amount": "29800000000000000", "minimumAmount": "29500000000000000"},
            "timeEstimate": 2
        },
        "fees": {"relayer": {
            "amount": "10000000000000",
            "currency": {"chainId": 56, "address": "0x0000000000000000000000000000000000000000"}
        }}
    })
}

async fn relay(polls: Arc<AtomicUsize>) -> Url {
    serve(move |request| {
        if request.path().starts_with("/intents/status/v3") {
            let status = if polls.fetch_add(1, Ordering::SeqCst) < 2 {
                "pending"
            } else {
                "success"
            };
            return Reply::json(200, &json!({"status": status}));
        }
        Reply::json(200, &quote(&request.json()))
    })
    .await
}

#[tokio::test]
async fn a_cross_chain_swap_is_quoted_signed_as_a_plain_deposit_and_tracked_to_done() {
    let scratch = Scratch::new();
    let sent = Arc::new(Mutex::new(Vec::new()));
    let shared = Arc::clone(&sent);
    let url = common::node(move |call| (node(&shared, call), Duration::ZERO)).await;
    let dead = Url::parse("http://127.0.0.1:9/").unwrap();
    let registry = Registry::with_networks(vec![
        common::network("bsc", url),
        common::network("base", dead.clone()),
    ])
    .unwrap();
    let mut providers = Providers::builtin().unwrap();
    providers.relay.url = relay(Arc::new(AtomicUsize::new(0))).await;
    providers.oneclick.url = dead;
    let mut config = config(&scratch, registry, None);
    config.swaps = Some(providers);
    let engine = Engine::start(config).unwrap();
    let mut events = engine.events();
    engine.create(SecretString::from("pw")).await.unwrap();
    let wallet = engine
        .import(SecretString::from(HARDHAT), SecretString::from(""))
        .await
        .unwrap();
    let request = SwapRequest {
        wallet,
        account: 0,
        sell: "bsc".parse().unwrap(),
        buy: "base".parse().unwrap(),
        amount: Amount::parse("0.1", 18).unwrap(),
    };
    let draft = engine.prepare_swap(request).await.unwrap();
    assert_eq!(draft.routes.len(), 1);
    assert_eq!(draft.unavailable.len(), 1);
    assert_eq!(draft.unavailable[0].provider, "NEAR Intents");
    let route = &draft.routes[0].route;
    assert_eq!((route.provider, route.method), ("Relay", Method::Deposit));
    assert_eq!(route.expected.to_string(), "0.0298");
    assert_eq!(route.minimum.to_string(), "0.0295");
    let network = route
        .costs
        .iter()
        .find(|cost| cost.kind == CostKind::NetworkFee)
        .unwrap();
    assert_eq!(network.charged, Charged::Extra);
    assert_eq!(network.amount.to_string(), "0.000042");
    let review = engine.choose_swap(draft.id, 0).await.unwrap();
    let deposit = Address::parse(ChainFamily::Evm, DEPOSIT).unwrap();
    assert_eq!(review.draft.transfer.to, deposit);
    assert_eq!(review.draft.transfer.amount.to_string(), "0.1");
    assert_eq!(review.draft.transfer.memo, None);
    assert_eq!(review.order.reference, REQUEST);
    assert!(engine.choose_swap(draft.id, 1).await.is_err());
    let tx = engine
        .confirm_send(review.draft.id, SecretString::from("pw"))
        .await
        .unwrap();
    assert_eq!(tx.to, deposit);
    assert_eq!(sent.lock().unwrap().len(), 1);
    let done = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Ok(Event::SwapUpdated(swap)) = events.recv().await
                && swap.status == SwapStatus::Done
            {
                return swap;
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(done.id, tx.id);
    assert_eq!(done.provider, "relay");
    assert_eq!(done.reference, REQUEST);
    assert_eq!(done.deposit, deposit);
    assert_eq!(done.expected.to_string(), "0.0298");
    engine.lock().await.unwrap();
    assert!(engine.swaps().await.is_err());
    engine.unlock(SecretString::from("pw")).await.unwrap();
    let kept = engine.swaps().await.unwrap();
    assert_eq!(kept.len(), 1);
    assert_eq!(kept[0].status, SwapStatus::Done);
}
