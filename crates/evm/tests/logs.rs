#![allow(clippy::unwrap_used)]

mod common;

use std::sync::{Arc, Mutex};

use bahaa_chain::{Incoming, ScanCursor};
use bahaa_config::{Endpoint, Protocol};
use bahaa_evm::EvmChain;
use bahaa_types::Address;
use common::{Log, USDT, assets, bsc_on, node, owner, result};
use serde_json::{Value, json};

const TRANSFER: &str = "0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef";
const SENDER: &str = "0x491c3e907cf37cf83bc6fcdfcd195f47ac05da52";
const TOKENS: [&str; 3] = [
    "bsc",
    "bsc/0x55d398326f99059fF775485246999027B3197955",
    "bsc/0x8AC76a51cc950d9822D68b83fE1Ad97B32Cd580d",
];

type Asked = Arc<Mutex<Vec<Value>>>;

fn padded(address: &str) -> String {
    format!(
        "0x{:0>64}",
        address.trim_start_matches("0x").to_ascii_lowercase()
    )
}

fn transfer_log(block: u64, value: u128, timestamp: bool) -> Value {
    moved(block, value, timestamp, (SENDER, common::OWNER))
}

fn moved(block: u64, value: u128, timestamp: bool, (from, to): (&str, &str)) -> Value {
    let mut log = json!({
        "address": USDT.to_ascii_lowercase(),
        "topics": [TRANSFER, padded(from), padded(to)],
        "data": format!("0x{value:064x}"),
        "blockNumber": format!("0x{block:x}"),
        "transactionHash": format!("0x{block:064x}"),
        "logIndex": "0x2",
        "removed": false
    });
    if timestamp {
        log["blockTimestamp"] = json!(format!("0x{:x}", block * 3));
    }
    log
}

fn number(value: &Value) -> u64 {
    u64::from_str_radix(value.as_str().unwrap().trim_start_matches("0x"), 16).unwrap()
}

async fn ledger(log: Log, asked: Asked, head: u64) -> url::Url {
    node(log, move |method, params| match method {
        "eth_chainId" => result(&json!("0x38")),
        "eth_blockNumber" => result(&json!(format!("0x{head:x}"))),
        "eth_getBlockByNumber" => result(&json!({"timestamp": format!("0x{:x}", number(&params[0]) * 2)})),
        "eth_getLogs" => {
            let filter = params[0].clone();
            let (from, to) = (number(&filter["fromBlock"]), number(&filter["toBlock"]));
            let sending = filter["topics"][1] == json!(padded(common::OWNER));
            asked.lock().unwrap().push(filter);
            let logs: Vec<Value> = if sending {
                [7_000]
                    .into_iter()
                    .filter(|block| (from..=to).contains(block))
                    .map(|block| moved(block, 1_000_000_000_000_000_000, true, (common::OWNER, SENDER)))
                    .collect()
            } else {
                [(6_000, true), (9_000, false)]
                    .into_iter()
                    .filter(|(block, _)| (from..=to).contains(block))
                    .map(|(block, timestamp)| transfer_log(block, 2_500_000_000_000_000_000, timestamp))
                    .collect()
            };
            result(&json!(logs))
        }
        _ => (200, json!({"jsonrpc": "2.0", "id": 1, "error": {"code": -32601, "message": "no such method"}})),
    })
    .await
}

#[tokio::test]
async fn a_first_scan_reaches_back_in_chunks_and_fills_missing_times() {
    let (log, asked) = (Log::default(), Asked::default());
    let chain = EvmChain::new(bsc_on(&[ledger(log, Arc::clone(&asked), 10_000).await])).unwrap();
    let scanned = chain
        .scan(&owner(), &assets(&TOKENS), None)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        scanned.cursor,
        ScanCursor {
            next: 9_989,
            complete_from: 5_989,
            complete_since: 11_978
        }
    );
    let found: Vec<(u64, u64, String, String)> = scanned
        .found
        .iter()
        .map(|entry| {
            (
                entry.block,
                entry.time,
                entry.amount.to_string(),
                entry.asset.to_string(),
            )
        })
        .collect();
    let usdt = format!("bsc/{USDT}");
    assert_eq!(
        found,
        [
            (6_000, 18_000, "2.5".to_owned(), usdt.clone()),
            (9_000, 18_000, "2.5".to_owned(), usdt)
        ]
    );
    assert_eq!(
        scanned.found[0].from,
        Address::parse(bahaa_types::ChainFamily::Evm, SENDER).unwrap()
    );
    assert_eq!((scanned.found[0].to, scanned.found[0].index), (owner(), 2));
    assert_eq!(scanned.found[0].id.as_str(), format!("0x{:064x}", 6_000));
    let asked = asked.lock().unwrap();
    let ranges: Vec<(u64, u64)> = asked
        .iter()
        .map(|filter| (number(&filter["fromBlock"]), number(&filter["toBlock"])))
        .collect();
    assert_eq!(
        ranges,
        [
            (5_989, 7_988),
            (5_989, 7_988),
            (7_989, 9_988),
            (7_989, 9_988)
        ]
    );
    assert_eq!(asked[0]["address"].as_array().unwrap().len(), 2);
    assert_eq!(asked[0]["topics"][2], json!(padded(common::OWNER)));
    assert_eq!(asked[0]["topics"][1], Value::Null);
    assert_eq!(asked[1]["topics"][1], json!(padded(common::OWNER)));
    assert_eq!(asked[1]["topics"].as_array().unwrap().len(), 2);
    let sent: Vec<(u64, String, Address)> = scanned
        .sent
        .iter()
        .map(|entry| (entry.block, entry.amount.to_string(), entry.to))
        .collect();
    assert_eq!(
        sent,
        [(
            7_000,
            "1".to_owned(),
            Address::parse(bahaa_types::ChainFamily::Evm, SENDER).unwrap()
        )]
    );
    assert_eq!(scanned.sent[0].from, owner());
}

#[tokio::test]
async fn later_scans_continue_from_the_cursor_and_wait_when_caught_up() {
    let (log, asked) = (Log::default(), Asked::default());
    let chain = EvmChain::new(bsc_on(&[ledger(
        Arc::clone(&log),
        Arc::clone(&asked),
        10_000,
    )
    .await]))
    .unwrap();
    let cursor = ScanCursor {
        next: 8_500,
        complete_from: 100,
        complete_since: 50,
    };
    let scanned = chain
        .scan(&owner(), &assets(&TOKENS), Some(cursor))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        scanned.cursor,
        ScanCursor {
            next: 9_989,
            ..cursor
        }
    );
    assert_eq!(scanned.found.len(), 1);
    assert_eq!(scanned.found[0].time, 18_000);
    assert_eq!(scanned.sent, Vec::new());
    assert_eq!(asked.lock().unwrap().len(), 2);
    let caught_up = chain
        .scan(&owner(), &assets(&TOKENS), Some(scanned.cursor))
        .await
        .unwrap();
    assert_eq!(caught_up, None);
    assert_eq!(asked.lock().unwrap().len(), 2);
    let natives_only = chain
        .scan(&owner(), &assets(&["bsc"]), None)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(natives_only.found, Vec::<Incoming>::new());
    assert_eq!(natives_only.cursor.next, 9_989);
    assert_eq!(asked.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn only_endpoints_that_serve_logs_are_asked() {
    let (plain_log, logs_log, asked) = (Log::default(), Log::default(), Asked::default());
    let plain = ledger(Arc::clone(&plain_log), Arc::clone(&asked), 10_000).await;
    let capable = ledger(Arc::clone(&logs_log), Arc::clone(&asked), 10_000).await;
    let mut network = bsc_on(&[plain, capable]);
    network.endpoints[0] = Endpoint {
        protocol: Protocol::JsonRpc,
        url: network.endpoints[0].url.clone(),
        features: Vec::new(),
        min_gap_ms: 0,
    };
    let chain = EvmChain::new(network.clone()).unwrap();
    assert!(chain.can_scan());
    chain
        .scan(&owner(), &assets(&TOKENS), None)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(*plain_log.lock().unwrap(), Vec::<String>::new());
    assert!(
        logs_log
            .lock()
            .unwrap()
            .iter()
            .any(|method| method == "eth_getLogs")
    );
    network.endpoints.truncate(1);
    assert!(!EvmChain::new(network).unwrap().can_scan());
}

#[tokio::test]
#[ignore = "talks to the real BSC network"]
async fn live_bsc_incoming_usdt() {
    let registry = bahaa_config::Registry::builtin().unwrap();
    let chain = EvmChain::new(registry.get(&"bsc".parse().unwrap()).unwrap().clone()).unwrap();
    let busy = Address::parse(
        bahaa_types::ChainFamily::Evm,
        "0x8894E0a0c962CB723c1976a4421c95949bE2D4E3",
    )
    .unwrap();
    let scanned = chain
        .scan(&busy, &assets(&TOKENS), None)
        .await
        .unwrap()
        .unwrap();
    assert!(scanned.cursor.complete_since > 1_700_000_000);
    assert!(
        !scanned.found.is_empty(),
        "no incoming USDT or USDC in the last 4000 blocks"
    );
    for entry in &scanned.found {
        assert_eq!(entry.to, busy);
        assert!(!entry.amount.is_zero());
        assert!((scanned.cursor.complete_from..scanned.cursor.next).contains(&entry.block));
    }
    assert!(
        !scanned.sent.is_empty(),
        "no outgoing USDT or USDC in the last 4000 blocks"
    );
    for entry in &scanned.sent {
        assert_eq!(entry.from, busy);
        assert!(!entry.amount.is_zero());
        assert!((scanned.cursor.complete_from..scanned.cursor.next).contains(&entry.block));
    }
}
