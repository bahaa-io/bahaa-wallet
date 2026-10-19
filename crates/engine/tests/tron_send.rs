#![allow(clippy::unwrap_used)]

mod common;

use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use bahaa_chain::FeeItem;
use bahaa_config::Registry;
use bahaa_crypto::SecretString;
use bahaa_engine::{Engine, Event, SendRequest, SentStatus, SentTx, Spend};
use bahaa_types::{Address, Amount, ChainFamily};
use common::{HARDHAT, Scratch, TRON_0_HEX, TRON_GENESIS, config};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::sync::broadcast;

const TRON_1: &str = "TPjjvMwjPoDC32V2dGDYTkLH4E5LAtBZ6C";
const TRON_1_HEX: &str = "419705bf55c3dcc6d277ebb8fe2a68762268822ba2";
const HEAD: u64 = 87_210_587;

#[derive(Default)]
struct Ledger {
    sent: Vec<String>,
    mined: bool,
}

fn now_ms() -> u64 {
    u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis(),
    )
    .unwrap()
}

fn txid_of(hex: &str) -> String {
    let bytes: Vec<u8> = (0..hex.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(&hex[at..at + 2], 16).unwrap())
        .collect();
    assert_eq!(bytes[0], 0x0a);
    let (mut length, mut at, mut shift) = (0usize, 1, 0);
    loop {
        length |= usize::from(bytes[at] & 0x7f) << shift;
        shift += 7;
        at += 1;
        if bytes[at - 1] & 0x80 == 0 {
            break;
        }
    }
    common::hex(&Sha256::digest(&bytes[at..at + length]))
}

fn answer(ledger: &Mutex<Ledger>, path: &str, body: &Value) -> Value {
    let mut ledger = ledger.lock().unwrap();
    match path {
        "/wallet/getblockbynum" => json!({"blockID": TRON_GENESIS}),
        "/wallet/getblock" => json!({
            "blockID": format!("{HEAD:016x}{}", "cd".repeat(24)),
            "block_header": {"raw_data": {"number": HEAD, "timestamp": now_ms()}}
        }),
        "/wallet/getaccount" => match body["address"].as_str().unwrap() {
            TRON_0_HEX => json!({"address": TRON_0_HEX, "balance": 5_000_000}),
            TRON_1_HEX => json!({"address": TRON_1_HEX}),
            _ => json!({}),
        },
        "/wallet/getaccountresource" => json!({"freeNetLimit": 600}),
        "/wallet/getchainparameters" => {
            let list: Vec<Value> = [
                ("getTransactionFee", 1_000_u64),
                ("getEnergyFee", 100),
                ("getCreateAccountFee", 100_000),
                ("getCreateNewAccountFeeInSystemContract", 1_000_000),
                ("getMaxFeeLimit", 15_000_000_000),
            ]
            .into_iter()
            .map(|(key, value)| json!({"key": key, "value": value}))
            .collect();
            json!({"chainParameter": list})
        }
        "/wallet/broadcasthex" => {
            let id = txid_of(body["transaction"].as_str().unwrap());
            ledger.sent.push(id.clone());
            json!({"result": true, "txid": id})
        }
        "/wallet/gettransactioninfobyid" => {
            let id = body["value"].as_str().unwrap();
            if ledger.mined && ledger.sent.iter().any(|sent| sent == id) {
                json!({"id": id, "blockNumber": HEAD + 2, "receipt": {"net_usage": 267}})
            } else {
                json!({})
            }
        }
        "/wallet/gettransactionfrompending" => {
            let id = body["value"].as_str().unwrap();
            if ledger.sent.iter().any(|sent| sent == id) {
                json!({"txID": id})
            } else {
                json!({})
            }
        }
        _ => json!({"Error": "unexpected path"}),
    }
}

async fn settled(events: &mut broadcast::Receiver<Event>, id: &str) -> SentTx {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Ok(Event::TxUpdated(tx)) = events.recv().await
                && tx.id == id
                && !tx.status.is_open()
            {
                return *tx;
            }
        }
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn trx_is_sent_from_the_wallets_tron_account_and_tracked_until_confirmed() {
    let scratch = Scratch::new();
    let ledger = Arc::new(Mutex::new(Ledger::default()));
    let shared = Arc::clone(&ledger);
    let url = common::rest_node(move |path, body| answer(&shared, path, body)).await;
    let registry = Registry::with_networks(vec![common::tron_network(url)]).unwrap();
    let engine = Engine::start(config(&scratch, registry, None)).unwrap();
    let mut events = engine.events();
    engine.create(SecretString::from("pw")).await.unwrap();
    let wallet = engine
        .import(SecretString::from(HARDHAT), SecretString::from(""))
        .await
        .unwrap();
    let request = SendRequest {
        account: 0,
        wallet,
        asset: "tron".parse().unwrap(),
        to: Address::parse(ChainFamily::Tron, TRON_1).unwrap(),
        amount: Spend::Exact(Amount::parse("1.5", 6).unwrap()),
        memo: None,
    };
    let draft = engine.prepare_send(request).await.unwrap();
    assert!(matches!(
        draft.fee.lines(),
        [line] if matches!(line.item, FeeItem::Bandwidth { .. }) && line.amount.is_zero()
    ));
    let sent = engine
        .confirm_send(draft.id, SecretString::from("pw"))
        .await
        .unwrap();
    assert_eq!(sent.status, SentStatus::Pending);
    assert_eq!(sent.sequence, None);
    assert_eq!(sent.from.to_string(), common::TRON_0);
    assert_eq!(ledger.lock().unwrap().sent, std::slice::from_ref(&sent.id));
    let expires = sent.expires_at.unwrap();
    let now = now_ms() / 1000;
    assert!(
        (now + 290..=now + 300).contains(&expires),
        "{expires} vs {now}"
    );
    ledger.lock().unwrap().mined = true;
    let done = settled(&mut events, &sent.id).await;
    assert_eq!(done.status, SentStatus::Confirmed);
    assert_eq!(done.block, Some(HEAD + 2));
    assert_eq!(done.fee_paid, Some(Amount::parse("0", 6).unwrap()));
    assert_eq!(done.raw, None);
    assert_eq!(engine.history().await.unwrap(), [done]);
}
