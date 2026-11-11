#![allow(clippy::unwrap_used)]

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use bahaa_chain::Movement;
use bahaa_evm::EvmChain;
use bahaa_types::{Address, ChainFamily};
use common::{Log, assets, bsc_on, node, owner, result};
use serde_json::{Value, json};

const SENDER: &str = "0x491c3e907cf37cf83bc6fcdfcd195f47ac05da52";
const ROUTER: &str = "0x3333333333333333333333333333333333333333";
const STRANGER: &str = "0x4444444444444444444444444444444444444444";
const ETHER: u128 = 1_000_000_000_000_000_000;
const KEPT_FROM: u64 = 6_500;

fn balance(block: u64) -> u128 {
    match block {
        0..6_000 => 0,
        6_000..7_000 => ETHER,
        7_000..8_000 => ETHER + ETHER / 2,
        8_000..9_000 => ETHER + ETHER * 3 / 4,
        9_000..10_200 => ETHER + ETHER * 3 / 4 - ETHER / 10 - ETHER / 1_000,
        _ => 2 * ETHER - ETHER / 10 - ETHER / 1_000 + ETHER / 20,
    }
}

fn nonce(block: u64) -> u64 {
    u64::from(block >= 9_000)
}

fn id(block: u64) -> String {
    format!("0x{block:064x}")
}

fn transaction(block: u64, from: &str, to: &str, value: u128, input: &str) -> Value {
    json!({
        "hash": id(block),
        "from": from.to_ascii_lowercase(),
        "to": to.to_ascii_lowercase(),
        "value": format!("0x{value:x}"),
        "input": input,
    })
}

fn transactions(block: u64) -> Vec<Value> {
    let owner = common::OWNER;
    match block {
        7_000 => vec![
            json!({"hash": format!("0x{:064x}", 1), "from": STRANGER, "to": ROUTER, "value": "0x5", "input": "0x"}),
            transaction(7_000, SENDER, owner, ETHER / 2, "0x6869"),
        ],
        8_000 => vec![transaction(8_000, STRANGER, ROUTER, 0, "0xabcdef01")],
        9_000 => vec![transaction(9_000, owner, SENDER, ETHER / 10, "0x")],
        10_200 => vec![transaction(10_200, SENDER, owner, ETHER / 20, "0x")],
        _ => Vec::new(),
    }
}

fn number(value: &Value) -> u64 {
    u64::from_str_radix(value.as_str().unwrap().trim_start_matches("0x"), 16).unwrap()
}

fn kept(block: u64, answer: &Value) -> (u16, Value) {
    if block < KEPT_FROM {
        (
            200,
            json!({"jsonrpc": "2.0", "id": 1, "error": {"code": -32000, "message": "missing trie node 1f2e"}}),
        )
    } else {
        result(answer)
    }
}

async fn chain(head: Arc<AtomicU64>) -> EvmChain {
    let url = node(Log::default(), move |method, params| match method {
        "eth_chainId" => result(&json!("0x38")),
        "eth_blockNumber" => result(&json!(format!("0x{:x}", head.load(Ordering::SeqCst)))),
        "eth_getBalance" => {
            let block = number(&params[1]);
            kept(block, &json!(format!("0x{:x}", balance(block))))
        }
        "eth_getTransactionCount" => {
            let block = number(&params[1]);
            kept(block, &json!(format!("0x{:x}", nonce(block))))
        }
        "eth_getBlockByNumber" => {
            let block = number(&params[0]);
            result(&json!({
                "number": format!("0x{block:x}"),
                "timestamp": format!("0x{:x}", block * 2),
                "transactions": transactions(block),
            }))
        }
        "eth_getBlockReceipts" if number(&params[0]) == 8_000 => result(&json!([{
            "transactionHash": id(8_000),
            "to": ROUTER,
            "logs": [{
                "topics": [format!("0x{:064x}", 9)],
                "data": format!("0x{:0>64}{:064x}", common::OWNER[2..].to_ascii_lowercase(), 7),
            }],
        }])),
        "eth_getBlockReceipts" => result(&json!([])),
        _ => (200, json!({"jsonrpc": "2.0", "id": 1, "error": {"code": -32601, "message": "no such method"}})),
    })
    .await;
    EvmChain::new(bsc_on(&[url])).unwrap()
}

fn evm(text: &str) -> Address {
    Address::parse(ChainFamily::Evm, text).unwrap()
}

fn summary(moved: &Movement) -> (String, Address, Address, String, u64, Option<String>) {
    (
        moved.id.to_string(),
        moved.from,
        moved.to,
        moved.amount.to_string(),
        moved.time,
        moved.memo.clone(),
    )
}

#[tokio::test]
async fn coins_that_arrive_or_leave_are_found_by_watching_the_balance() {
    let head = Arc::new(AtomicU64::new(10_000));
    let chain = chain(Arc::clone(&head)).await;
    let first = chain
        .scan(&owner(), &assets(&["bsc"]), None, None)
        .await
        .unwrap()
        .unwrap();
    let found: Vec<_> = first.found.iter().map(summary).collect();
    assert_eq!(
        found,
        [
            (
                id(7_000),
                evm(SENDER),
                owner(),
                "0.5".to_owned(),
                14_000,
                Some("hi".to_owned())
            ),
            (
                id(8_000),
                evm(ROUTER),
                owner(),
                "0.25".to_owned(),
                16_000,
                None
            ),
        ]
    );
    let sent: Vec<_> = first.sent.iter().map(summary).collect();
    assert_eq!(
        sent,
        [(
            id(9_000),
            owner(),
            evm(SENDER),
            "0.1".to_owned(),
            18_000,
            None
        )]
    );
    assert!(
        first
            .found
            .iter()
            .chain(&first.sent)
            .all(|moved| moved.asset == "bsc".parse().unwrap())
    );
    let watch = first.watch.unwrap();
    assert_eq!((watch.block, watch.nonce), (9_988, 1));
    assert_eq!(watch.balance.to_string(), "1.649");

    let quiet = chain
        .scan(&owner(), &assets(&["bsc"]), Some(first.cursor), first.watch)
        .await
        .unwrap();
    assert_eq!(quiet, None);

    head.store(10_500, Ordering::SeqCst);
    let next = chain
        .scan(&owner(), &assets(&["bsc"]), Some(first.cursor), first.watch)
        .await
        .unwrap()
        .unwrap();
    let found: Vec<_> = next.found.iter().map(summary).collect();
    assert_eq!(
        found,
        [(
            id(10_200),
            evm(SENDER),
            owner(),
            "0.05".to_owned(),
            20_400,
            None
        )]
    );
    assert_eq!(next.sent, Vec::<Movement>::new());
    assert_eq!(next.watch.unwrap().block, 10_488);
}

#[tokio::test]
async fn without_past_balances_the_watch_starts_from_now_and_token_history_still_runs() {
    let url = node(Log::default(), move |method, _| match method {
        "eth_chainId" => result(&json!("0x38")),
        "eth_blockNumber" => result(&json!("0x2710")),
        "eth_getBlockByNumber" => result(&json!({"timestamp": "0x64"})),
        _ => (200, json!({"jsonrpc": "2.0", "id": 1, "error": {"code": -32601, "message": "no such method"}})),
    })
    .await;
    let chain = EvmChain::new(bsc_on(&[url])).unwrap();
    let scanned = chain
        .scan(&owner(), &assets(&["bsc"]), None, None)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(scanned.found, Vec::<Movement>::new());
    assert_eq!(scanned.sent, Vec::<Movement>::new());
    assert_eq!(scanned.watch, None);
    assert_eq!(scanned.cursor.next, 9_989);
}
