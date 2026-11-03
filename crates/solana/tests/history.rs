#![allow(clippy::unwrap_used)]

mod common;

use std::sync::{Arc, Mutex};

use bahaa_chain::ScanCursor;
use bahaa_solana::SolanaChain;
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use common::{Ledger, address, called, native, node, solana_on};
use serde_json::{Value, json};

const BINANCE: &str = "9WzDXwBbmkg8ZTbNMqUxvQRAyrZzDsGYdLVL9zYtAWWM";

fn recorded() -> Value {
    serde_json::from_str(include_str!("fixtures/history.json")).unwrap()
}

fn signature(found: &Value) -> String {
    let raw = STANDARD
        .decode(found["transaction"][0].as_str().unwrap())
        .unwrap();
    bs58::encode(&raw[1..65]).into_string()
}

fn listed(signature: &str, time: u64, err: &Value) -> Value {
    json!({"signature": signature, "slot": 1, "blockTime": time, "err": err, "memo": null, "confirmationStatus": "finalized"})
}

#[tokio::test]
async fn history_is_read_newest_first_and_failed_transfers_are_not_fetched() {
    let coins = recorded()["coins"].clone();
    let id = signature(&coins);
    let time = coins["blockTime"].as_u64().unwrap();
    let mut ledger = Ledger::default();
    ledger.transactions.insert(id.clone(), coins);
    let failed =
        "4UsiwZTYEBqNpUnYAteKAKd2dC1QiaFBNLBpxAWzLLhTAAwTWJb6BHH6FMTBfmbMorNGpruvGnLzzoY7dYU2vyhY";
    ledger.history = vec![
        listed(&id, time, &Value::Null),
        listed(
            failed,
            time - 60,
            &json!({"InstructionError": [0, "InvalidArgument"]}),
        ),
    ];
    let log = Arc::new(Mutex::new(Vec::new()));
    let chain = SolanaChain::new(solana_on(&[node(Arc::clone(&log), ledger).await])).unwrap();
    assert!(chain.can_scan());
    let scanned = chain
        .scan(&address(BINANCE), &[native()], None)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(scanned.found.len(), 1);
    assert_eq!(scanned.sent, []);
    assert_eq!(scanned.found[0].id.as_str(), id);
    assert_eq!(scanned.found[0].amount.to_string(), "0.000001");
    assert_eq!(
        scanned.cursor,
        ScanCursor {
            next: time - 300,
            complete_from: time - 300,
            complete_since: time - 300,
        }
    );
    assert_eq!(called(&log, "getTransaction").len(), 1);
    let listing = &called(&log, "getSignaturesForAddress")[0];
    assert_eq!(listing[0], BINANCE);
    assert_eq!(listing[1]["commitment"], "finalized");
    let again = chain
        .scan(&address(BINANCE), &[native()], Some(scanned.cursor))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(again.cursor, scanned.cursor);
    assert_eq!(again.found, scanned.found);
}

#[tokio::test]
async fn a_quiet_address_has_no_history_to_record() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let chain = SolanaChain::new(solana_on(&[node(log, Ledger::default()).await])).unwrap();
    let scanned = chain
        .scan(&address(BINANCE), &[native()], None)
        .await
        .unwrap()
        .unwrap();
    assert!(scanned.found.is_empty() && scanned.sent.is_empty());
    let quiet = chain
        .scan(&address(BINANCE), &[native()], Some(scanned.cursor))
        .await
        .unwrap();
    assert_eq!(quiet, None);
}
