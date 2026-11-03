#![allow(clippy::unwrap_used)]

mod common;

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bahaa_config::Registry;
use bahaa_crypto::SecretString;
use bahaa_engine::SentKind;
use bahaa_engine::swap::{Charged, CostKind, Method, Providers, Step, SwapStatus};
use bahaa_engine::{Engine, Event, Spend, SwapRequest, WalletId};
use bahaa_testkit::{Reply, serve};
use bahaa_types::{Address, Amount, ChainFamily};
use common::{HARDHAT, HARDHAT_0, Scratch, USDT, config, result, unhex};
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
        "eth_estimateGas" if call["params"][0]["to"] == DEPOSIT => json!("0x7530"),
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

type Sent = Arc<Mutex<Vec<String>>>;

async fn swapping(scratch: &Scratch) -> (Engine, WalletId, Sent) {
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
    let mut config = config(scratch, registry, None);
    config.swaps = Some(providers);
    let engine = Engine::start(config).unwrap();
    engine.create(SecretString::from("pw")).await.unwrap();
    let wallet = engine
        .import(SecretString::from(HARDHAT), SecretString::from(""))
        .await
        .unwrap();
    (engine, wallet, sent)
}

fn bnb_to_base(wallet: WalletId, amount: Spend) -> SwapRequest {
    SwapRequest {
        wallet,
        account: 0,
        sell: "bsc".parse().unwrap(),
        buy: "base".parse().unwrap(),
        amount,
    }
}

#[tokio::test]
async fn swapping_everything_quotes_the_exact_balance_left_after_the_network_fee() {
    let scratch = Scratch::new();
    let (engine, wallet, _sent) = swapping(&scratch).await;
    let draft = engine
        .prepare_swap(bnb_to_base(wallet, Spend::Max))
        .await
        .unwrap();
    assert_eq!(draft.amount.to_string(), "9.999937");
    assert_eq!(draft.routes[0].route.amount, draft.amount);
    let review = engine.choose_swap(draft.id, 0).await.unwrap();
    assert_eq!(review.draft.transfer.amount.to_string(), "9.999892");
    assert!(!review.draft.transfer.max);
    assert_eq!(review.order.amount, review.draft.transfer.amount);
    assert_eq!(review.route.route.amount, review.draft.transfer.amount);
}

#[tokio::test]
async fn a_cross_chain_swap_is_quoted_signed_as_a_plain_deposit_and_tracked_to_done() {
    let scratch = Scratch::new();
    let (engine, wallet, sent) = swapping(&scratch).await;
    let mut events = engine.events();
    let request = SwapRequest {
        wallet,
        account: 0,
        sell: "bsc".parse().unwrap(),
        buy: "base".parse().unwrap(),
        amount: Spend::Exact(Amount::parse("0.1", 18).unwrap()),
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
    assert_eq!(network.amount.to_string(), "0.00006");
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

const QUOTER: &str = "0xb048bbc1ee6b733fffcfb9e9cef7375518e25997";
const PANCAKE: &str = "0x13f4ea83d0bd40e75c8222255bc855a974568dd4";
const POOL: &str = "0x172fcd41e0913e95784454622d1c3724f546f849";
const NATIVE: &str = "0xeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee";
const TRANSFER: &str = "0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef";

fn topic(address: &str) -> String {
    format!("0x{:0>64}", address.trim_start_matches("0x").to_lowercase())
}

fn word(units: u128) -> String {
    format!("0x{units:064x}")
}

fn simulated(calls: &[Value], paid: u64) -> Value {
    let me = HARDHAT_0.to_lowercase();
    let answers: Vec<Value> = calls
        .iter()
        .map(|call| {
            let data = call["data"].as_str().unwrap_or_default();
            let logs = if call["to"] == PANCAKE {
                json!([
                    {"address": USDT, "topics": [TRANSFER, topic(&me), topic(POOL)], "data": word(50_000_000_000_000_000_000)},
                    {"address": NATIVE, "topics": [TRANSFER, topic(PANCAKE), topic(&me)], "data": word(u128::from(paid))},
                ])
            } else {
                json!([])
            };
            let output = if data.starts_with("0xdd62ed3e") { word(0) } else { "0x".to_owned() };
            json!({"status": "0x1", "gasUsed": "0x249f0", "returnData": output, "logs": logs})
        })
        .collect();
    json!([{"calls": answers}])
}

fn pool_node(sent: &Sent, call: &Value, paid: &AtomicU64) -> Value {
    let first = call["params"][0].as_str().unwrap_or_default().to_owned();
    let mut sent = sent.lock().unwrap();
    let hash = |raw: &str| {
        format!(
            "0x{}",
            common::hex(&Keccak256::digest(unhex(raw.trim_start_matches("0x"))))
        )
    };
    let known = sent.iter().any(|raw| hash(raw) == first);
    let target = call["params"][0]["to"]
        .as_str()
        .unwrap_or_default()
        .to_lowercase();
    let data = call["params"][0]["data"].as_str().unwrap_or_default();
    let reply = match call["method"].as_str().unwrap() {
        "eth_chainId" => json!("0x38"),
        "eth_getBalance" => json!("0x8ac7230489e80000"),
        "eth_getTransactionCount" => json!("0x0"),
        "eth_feeHistory" => {
            json!({"oldestBlock": "0x1", "baseFeePerGas": ["0x3b9aca00", "0x3b9aca00"]})
        }
        "eth_maxPriorityFeePerGas" => json!("0x3b9aca00"),
        "eth_call" if target == QUOTER => json!(format!(
            "{}{}",
            word(80_000_000_000_000_000),
            "0".repeat(192)
        )),
        "eth_call" if target == USDT && data.starts_with("0x313ce567") => json!(word(18)),
        "eth_call" if target == USDT && data.starts_with("0x70a08231") => {
            json!(word(100_000_000_000_000_000_000))
        }
        "eth_call" => json!(word(0)),
        "eth_simulateV1" => {
            let calls = call["params"][0]["blockStateCalls"][0]["calls"]
                .as_array()
                .unwrap()
                .clone();
            simulated(&calls, paid.load(Ordering::SeqCst))
        }
        "eth_sendRawTransaction" => {
            sent.push(first.clone());
            json!(hash(&first))
        }
        "eth_getTransactionReceipt" if known => json!({
            "status": "0x1",
            "blockNumber": "0x10",
            "gasUsed": "0x249f0",
            "effectiveGasPrice": "0x77359400"
        }),
        "eth_getTransactionByHash" if known => json!({"hash": first}),
        _ => Value::Null,
    };
    result(reply)
}

async fn pooled(scratch: &Scratch, paid: &Arc<AtomicU64>) -> (Engine, WalletId, Sent) {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let shared = Arc::clone(&sent);
    let paid = Arc::clone(paid);
    let url = common::node(move |call| (pool_node(&shared, call, &paid), Duration::ZERO)).await;
    let dead = Url::parse("http://127.0.0.1:9/").unwrap();
    let registry = Registry::with_networks(vec![common::network("bsc", url)]).unwrap();
    let mut providers = Providers::builtin().unwrap();
    providers.relay.url = dead.clone();
    providers.kyberswap.url = dead.clone();
    providers.velora.url = dead.clone();
    providers.oneclick.url = dead;
    let mut config = config(scratch, registry, None);
    config.swaps = Some(providers);
    let engine = Engine::start(config).unwrap();
    engine.create(SecretString::from("pw")).await.unwrap();
    let wallet = engine
        .import(SecretString::from(HARDHAT), SecretString::from(""))
        .await
        .unwrap();
    (engine, wallet, sent)
}

fn usdt_to_bnb(wallet: WalletId) -> SwapRequest {
    SwapRequest {
        wallet,
        account: 0,
        sell: format!("bsc/{USDT}").parse().unwrap(),
        buy: "bsc".parse().unwrap(),
        amount: Spend::Exact(Amount::parse("50", 18).unwrap()),
    }
}

#[tokio::test]
async fn a_pool_swap_approves_exactly_is_checked_by_simulation_and_settles_when_mined() {
    let scratch = Scratch::new();
    let paid = Arc::new(AtomicU64::new(80_000_000_000_000_000));
    let (engine, wallet, sent) = pooled(&scratch, &paid).await;
    let mut events = engine.events();
    let draft = engine.prepare_swap(usdt_to_bnb(wallet)).await.unwrap();
    let index = draft
        .routes
        .iter()
        .position(|ranked| ranked.route.provider == "PancakeSwap")
        .unwrap();
    let route = &draft.routes[index].route;
    assert_eq!(route.method, Method::Pool);
    assert_eq!(route.expected.to_string(), "0.08");
    assert_eq!(route.minimum.to_string(), "0.0796");
    assert!(
        route
            .costs
            .iter()
            .any(|cost| cost.kind == CostKind::PoolFee)
    );
    let network = route
        .costs
        .iter()
        .find(|cost| cost.kind == CostKind::NetworkFee)
        .unwrap();
    assert_eq!(network.amount.to_string(), "0.0006");
    let review = engine.choose_swap(draft.id, index).await.unwrap();
    assert_eq!(review.steps.len(), 2);
    assert!(matches!(
        &review.steps[0],
        Step::Approve { amount, spender, .. }
            if amount.to_string() == "50" && spender.to_string().to_lowercase() == PANCAKE
    ));
    assert_eq!(review.steps[1], Step::Swap);
    assert!(review.checked);
    assert_eq!(review.draft.transfer.to.to_string().to_lowercase(), PANCAKE);
    let tx = engine
        .confirm_send(review.draft.id, SecretString::from("pw"))
        .await
        .unwrap();
    assert_eq!(tx.kind, SentKind::Swap);
    assert!(tx.private);
    let raw = sent.lock().unwrap().clone();
    assert_eq!(raw.len(), 2);
    assert!(raw[0].contains("095ea7b3"), "the approval goes first");
    let history = engine.history().await.unwrap();
    let approval = history
        .iter()
        .find(|record| record.kind == SentKind::Approval)
        .unwrap();
    assert_eq!(approval.amount.to_string(), "50");
    assert_eq!(approval.sequence, Some(0));
    assert_eq!(tx.sequence, Some(1));
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
    assert_eq!(
        (done.provider.as_str(), done.reference.as_str()),
        ("pancakeswap", tx.id.as_str())
    );
}

#[tokio::test]
async fn a_pool_swap_that_would_pay_less_than_its_minimum_is_listed_as_unavailable() {
    let scratch = Scratch::new();
    let paid = Arc::new(AtomicU64::new(70_000_000_000_000_000));
    let (engine, wallet, _sent) = pooled(&scratch, &paid).await;
    let draft = engine.prepare_swap(usdt_to_bnb(wallet)).await.unwrap();
    assert!(
        draft
            .routes
            .iter()
            .all(|ranked| ranked.route.provider != "PancakeSwap")
    );
    let missing = draft
        .unavailable
        .iter()
        .find(|missing| missing.provider == "PancakeSwap")
        .unwrap();
    assert_eq!(missing.reason, "the swap would pay less than its minimum");
}

#[tokio::test]
async fn a_pool_swap_whose_price_moved_after_the_review_is_refused_at_signing() {
    let scratch = Scratch::new();
    let paid = Arc::new(AtomicU64::new(80_000_000_000_000_000));
    let (engine, wallet, sent) = pooled(&scratch, &paid).await;
    let draft = engine.prepare_swap(usdt_to_bnb(wallet)).await.unwrap();
    let index = draft
        .routes
        .iter()
        .position(|ranked| ranked.route.provider == "PancakeSwap")
        .unwrap();
    let review = engine.choose_swap(draft.id, index).await.unwrap();
    paid.store(70_000_000_000_000_000, Ordering::SeqCst);
    let refused = engine
        .confirm_send(review.draft.id, SecretString::from("pw"))
        .await
        .unwrap_err();
    assert_eq!(
        refused.to_string(),
        "the swap would pay less than its minimum"
    );
    assert_eq!(sent.lock().unwrap().len(), 0);
    assert_eq!(engine.history().await.unwrap().len(), 0);
}

struct CowSide {
    placed: Mutex<Vec<Value>>,
    uid: Mutex<String>,
}

async fn cow(side: Arc<CowSide>) -> Url {
    serve(move |request| {
        let path = request.path().to_owned();
        if path.ends_with("/quote") {
            let body = request.json();
            let amount: u128 = body["sellAmountBeforeFee"]
                .as_str()
                .unwrap()
                .parse()
                .unwrap();
            let fee = 8_000_000_000_000_000u128;
            return Reply::json(
                200,
                &json!({
                    "quote": {
                        "sellToken": body["sellToken"],
                        "buyToken": body["buyToken"],
                        "receiver": body["receiver"],
                        "sellAmount": (amount - fee).to_string(),
                        "feeAmount": fee.to_string(),
                        "buyAmount": "71583606250152798",
                        "kind": "sell",
                        "validTo": 1_900_000_000u32,
                    },
                    "id": 7,
                }),
            );
        }
        if path.ends_with("/orders") {
            side.placed.lock().unwrap().push(request.json());
            return Reply::json(201, &json!(side.uid.lock().unwrap().clone()));
        }
        Reply::json(200, &json!({"status": "fulfilled"}))
    })
    .await
}

#[tokio::test]
async fn a_cow_order_is_signed_with_its_approval_and_placed_once_the_approval_confirms() {
    let scratch = Scratch::new();
    let sent = Arc::new(Mutex::new(Vec::new()));
    let shared = Arc::clone(&sent);
    let paid = Arc::new(AtomicU64::new(0));
    let node = common::node(move |call| (pool_node(&shared, call, &paid), Duration::ZERO)).await;
    let side = Arc::new(CowSide {
        placed: Mutex::new(Vec::new()),
        uid: Mutex::new(String::new()),
    });
    let dead = Url::parse("http://127.0.0.1:9/").unwrap();
    let mut providers = Providers::builtin().unwrap();
    providers.relay.url = dead.clone();
    providers.kyberswap.url = dead.clone();
    providers.velora.url = dead.clone();
    providers.oneclick.url = dead.clone();
    providers.cow.url = cow(Arc::clone(&side)).await;
    providers.pools.clear();
    let registry = Registry::with_networks(vec![common::network("bsc", node)]).unwrap();
    let mut config = config(&scratch, registry, None);
    config.swaps = Some(providers);
    let engine = Engine::start(config).unwrap();
    engine.create(SecretString::from("pw")).await.unwrap();
    let wallet = engine
        .import(SecretString::from(HARDHAT), SecretString::from(""))
        .await
        .unwrap();
    let mut events = engine.events();
    let draft = engine.prepare_swap(usdt_to_bnb(wallet)).await.unwrap();
    let index = draft
        .routes
        .iter()
        .position(|ranked| ranked.route.provider == "CoW Swap")
        .unwrap();
    let route = &draft.routes[index].route;
    assert_eq!(route.method, Method::Auction);
    assert_eq!(route.expected.to_string(), "0.071583606250152798");
    assert_eq!(route.minimum.to_string(), "0.071225688218902034");
    let costs: Vec<(CostKind, Charged)> = route
        .costs
        .iter()
        .map(|cost| (cost.kind, cost.charged))
        .collect();
    assert!(costs.contains(&(CostKind::NetworkFee, Charged::InQuote)));
    assert!(costs.contains(&(CostKind::NetworkFee, Charged::Extra)));
    let review = engine.choose_swap(draft.id, index).await.unwrap();
    assert_eq!(review.steps.len(), 2);
    assert!(matches!(review.steps[0], Step::Approve { .. }));
    assert!(review.order.typed.is_some());
    assert!(
        engine
            .confirm_send(review.draft.id, SecretString::from("pw"))
            .await
            .is_err()
    );
    let review = engine.choose_swap(draft.id, index).await.unwrap();
    let record = engine
        .confirm_order(review.draft.id, SecretString::from("pw"))
        .await
        .unwrap();
    assert_eq!(record.provider, "cow");
    assert_eq!(record.id, record.reference);
    assert_eq!(record.id.len(), 114);
    *side.uid.lock().unwrap() = record.id.clone();
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
    assert_eq!(done.id, record.id);
    assert_eq!(
        sent.lock().unwrap().len(),
        1,
        "only the approval is a transaction"
    );
    let placed = side.placed.lock().unwrap().clone();
    assert_eq!(placed.len(), 1);
    let order = &placed[0];
    assert_eq!(order["receiver"], HARDHAT_0.to_lowercase());
    assert_eq!(order["sellAmount"], "50000000000000000000");
    assert_eq!(order["buyAmount"], "71225688218902034");
    assert_eq!(order["feeAmount"], "0");
    assert_eq!(order["signature"].as_str().unwrap().len(), 132);
}
