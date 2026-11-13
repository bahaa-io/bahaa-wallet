#![allow(clippy::unwrap_used)]

mod common;

use std::sync::{Arc, Mutex};

use bahaa_chain::ScanCursor;
use bahaa_solana::SolanaChain;
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use common::{
    AAPLX, Ledger, TOKEN_2022, address, called, native, node, serve, solana_on, token_account_with,
    usdc,
};
use serde_json::{Value, json};

const BINANCE: &str = "9WzDXwBbmkg8ZTbNMqUxvQRAyrZzDsGYdLVL9zYtAWWM";
const BINANCE_AAPLX: &str = "DK2ZeJzewxjESATTTwQwh7GNx3QxhWbbLpqcgPVGryot";

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

#[tokio::test]
async fn a_refused_lookup_keeps_what_was_read_before_it() {
    let coins = recorded()["coins"].clone();
    let id = signature(&coins);
    let time = coins["blockTime"].as_u64().unwrap();
    let refused =
        "4UsiwZTYEBqNpUnYAteKAKd2dC1QiaFBNLBpxAWzLLhTAAwTWJb6BHH6FMTBfmbMorNGpruvGnLzzoY7dYU2vyhY";
    let mut ledger = Ledger::default();
    ledger.transactions.insert(id.clone(), coins);
    ledger.history = vec![
        listed(&id, time, &Value::Null),
        listed(refused, time - 60, &Value::Null),
    ];
    let limited = |ledger: Ledger| {
        move |method: &str, params: &Value| {
            if method == "getTransaction" && params[0] == refused {
                return (
                    429,
                    json!({"jsonrpc": "2.0", "id": 1, "error": {"code": 429, "message": "Too many requests"}}),
                );
            }
            (
                200,
                json!({"jsonrpc": "2.0", "id": 1, "result": ledger.answer(method, params)}),
            )
        }
    };
    let log = Arc::new(Mutex::new(Vec::new()));
    let chain = SolanaChain::new(solana_on(&[serve(
        Arc::clone(&log),
        limited(ledger.clone()),
    )
    .await]))
    .unwrap();
    let scanned = chain
        .scan(&address(BINANCE), &[native()], None)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(scanned.found.len(), 1);
    assert_eq!(scanned.found[0].id.as_str(), id);
    ledger.history.reverse();
    let chain = SolanaChain::new(solana_on(&[serve(log, limited(ledger)).await])).unwrap();
    assert!(
        chain
            .scan(&address(BINANCE), &[native()], None)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn token_histories_are_read_only_for_opened_token_accounts() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let chain = SolanaChain::new(solana_on(
        &[node(Arc::clone(&log), Ledger::default()).await],
    ))
    .unwrap();
    chain
        .scan(&address(BINANCE), &[native(), usdc()], None)
        .await
        .unwrap();
    let listings: Vec<Value> = called(&log, "getSignaturesForAddress")
        .into_iter()
        .map(|params| params[0].clone())
        .collect();
    assert_eq!(listings, [json!(BINANCE)]);
}

#[tokio::test]
async fn token_2022_histories_are_read_from_the_token_2022_account() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let mut ledger = Ledger::default();
    ledger.accounts.insert(
        BINANCE_AAPLX.to_owned(),
        token_account_with(TOKEN_2022, BINANCE, AAPLX, 5, &[(7, &[])]),
    );
    let chain = SolanaChain::new(solana_on(&[node(Arc::clone(&log), ledger).await])).unwrap();
    let aaplx = format!("solana/{AAPLX}").parse().unwrap();
    chain
        .scan(&address(BINANCE), &[native(), aaplx], None)
        .await
        .unwrap();
    let listings: Vec<Value> = called(&log, "getSignaturesForAddress")
        .into_iter()
        .map(|params| params[0].clone())
        .collect();
    assert_eq!(listings, [json!(BINANCE), json!(BINANCE_AAPLX)]);
}
