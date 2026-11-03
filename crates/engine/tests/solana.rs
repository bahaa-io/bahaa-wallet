#![allow(clippy::unwrap_used)]

mod common;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use bahaa_config::{Endpoint, Feature, Network, Protocol, Registry};
use bahaa_crypto::SecretString;
use bahaa_engine::{Engine, Event, SendRequest, SentStatus, SentTx, Spend, WalletKind};
use bahaa_types::{Address, Amount, AssetId, ChainFamily};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use common::{Scratch, config, result, until};
use serde_json::{Value, json};
use tokio::sync::broadcast;
use url::Url;

const PHRASE: &str =
    "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";
const OWNER: &str = "HAgk14JpMQLgt6rVgv7cBQFJWFto5Dqxi472uT3DKpqk";
const FRIEND: &str = "2bUBiBNZyD29gP1oV6de7nxowMLoDBtopMMTGgMvjG5m";
const SYSTEM: &str = "11111111111111111111111111111111";
const NETWORK: &str = r##"[{
    "id": "solana",
    "name": "Solana",
    "color": "#9945ff",
    "family": "solana",
    "genesis": "45296998a6f8e2a784db5d9f95e18fc23f70441a1039446801089879b08c7ef0",
    "testnet": false,
    "native": {"symbol": "SOL", "name": "Solana", "decimals": 9},
    "endpoints": [{"protocol": "solana-rpc", "url": "https://api.mainnet-beta.solana.com"}],
    "explorer": {"tx": "https://solscan.io/tx/{tx}", "address": "https://solscan.io/account/{address}"}
}]"##;

#[derive(Default)]
struct Ledger {
    sent: Vec<String>,
}

fn solana(url: Url) -> Network {
    let mut network = Registry::from_json(NETWORK)
        .unwrap()
        .get(&"solana".parse().unwrap())
        .unwrap()
        .clone();
    network.endpoints = vec![Endpoint {
        protocol: Protocol::SolanaRpc,
        url,
        features: vec![Feature::History],
        min_gap_ms: 0,
        history_gap_ms: 0,
    }];
    network
}

fn account(lamports: u64) -> Value {
    json!({"lamports": lamports, "owner": SYSTEM, "executable": false, "data": ["", "base64"], "space": 0})
}

fn answer(ledger: &Mutex<Ledger>, call: &Value) -> Value {
    let params = &call["params"];
    let mut ledger = ledger.lock().unwrap();
    result(match call["method"].as_str().unwrap() {
        "getGenesisHash" => json!("5eykt4UsFv8P8NJdTREpY1vzqKqZKvdpKuc147dw2N9d"),
        "getMultipleAccounts" => {
            let listed: Vec<Value> = params[0]
                .as_array()
                .unwrap()
                .iter()
                .map(|key| match key.as_str().unwrap() {
                    OWNER => account(1_250_000_000),
                    FRIEND => account(1),
                    _ => Value::Null,
                })
                .collect();
            json!({"context": {"slot": 1}, "value": listed})
        }
        "getMinimumBalanceForRentExemption" => json!(650_240),
        "getLatestBlockhash" => {
            json!({"context": {"slot": 1}, "value": {"blockhash": "Aiyo4Fg2XByNaEhKv3LtX5h9Q8Y2kzhhCZhsnV5fBsrw", "lastValidBlockHeight": 9}})
        }
        "getRecentPrioritizationFees" => json!([{"slot": 1, "prioritizationFee": 1_000}]),
        "simulateTransaction" => {
            json!({"context": {"slot": 1}, "value": {"err": null, "logs": [], "unitsConsumed": 450}})
        }
        "sendTransaction" => {
            let bytes = STANDARD.decode(params[0].as_str().unwrap()).unwrap();
            let id = bs58::encode(&bytes[1..65]).into_string();
            ledger.sent.push(params[0].as_str().unwrap().to_owned());
            json!(id)
        }
        "getSignatureStatuses" => {
            json!({"context": {"slot": 1}, "value": [{"slot": 77, "err": null, "confirmationStatus": "confirmed"}]})
        }
        "getTransaction" => {
            json!({"slot": 77, "meta": {"fee": 5_001, "err": null}, "transaction": [ledger.sent[0], "base64"]})
        }
        other => panic!("unexpected {other}"),
    })
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
async fn a_recovery_phrase_holds_sol_and_sends_it_within_the_quote_life() {
    let scratch = Scratch::new();
    let ledger = Arc::new(Mutex::new(Ledger::default()));
    let shared = Arc::clone(&ledger);
    let url = common::node(move |call| (answer(&shared, call), Duration::ZERO)).await;
    let registry = Registry::with_networks(vec![solana(url)]).unwrap();
    let engine = Engine::start(config(&scratch, registry, None)).unwrap();
    let mut events = engine.events();
    engine.create(SecretString::from("pw")).await.unwrap();
    let wallet = engine
        .import(SecretString::from(PHRASE), SecretString::from(""))
        .await
        .unwrap();
    let mut portfolio = engine.portfolio();
    let held = until(&mut portfolio, |portfolio| !portfolio.holdings.is_empty()).await;
    assert_eq!(held.holdings[0].address.to_string(), OWNER);
    assert_eq!(held.holdings[0].amount.to_string(), "1.25");
    let request = SendRequest {
        account: 0,
        wallet,
        asset: "solana".parse::<AssetId>().unwrap(),
        to: Address::parse(ChainFamily::Solana, FRIEND).unwrap(),
        amount: Spend::Exact(Amount::parse("0.1", 9).unwrap()),
        memo: Some("dinner".to_owned()),
    };
    let draft = engine.prepare_send(request).await.unwrap();
    assert_eq!(draft.valid_for, bahaa_solana::QUOTE_LIFE);
    assert_eq!(draft.fee.expected().to_string(), "0.000005001");
    let sent = engine
        .confirm_send(draft.id, SecretString::from("pw"))
        .await
        .unwrap();
    assert_eq!(sent.from.to_string(), OWNER);
    assert_eq!(sent.memo.as_deref(), Some("dinner"));
    assert_eq!(ledger.lock().unwrap().sent.len(), 1);
    let done = settled(&mut events, &sent.id).await;
    assert_eq!(done.status, SentStatus::Confirmed);
    assert_eq!(done.block, Some(77));
    assert_eq!(
        done.fee_paid,
        Some(Amount::parse("0.000005001", 9).unwrap())
    );
}

#[tokio::test]
async fn a_solana_key_and_a_watched_address_hold_only_their_account() {
    let scratch = Scratch::new();
    let ledger = Arc::new(Mutex::new(Ledger::default()));
    let url = common::node(move |call| (answer(&ledger, call), Duration::ZERO)).await;
    let registry = Registry::with_networks(vec![solana(url)]).unwrap();
    let engine = Engine::start(config(&scratch, registry, None)).unwrap();
    engine.create(SecretString::from("pw")).await.unwrap();
    let exported = bahaa_crypto::solana_keypair(&[0x9d; 32]);
    let key = engine
        .import_key(
            SecretString::from(exported.to_string()),
            ChainFamily::Solana,
        )
        .await
        .unwrap();
    let watched = engine
        .watch(Address::parse(ChainFamily::Solana, OWNER).unwrap(), true)
        .await
        .unwrap();
    let kinds: Vec<WalletKind> = engine
        .wallets()
        .await
        .unwrap()
        .iter()
        .map(|wallet| wallet.kind)
        .collect();
    assert_eq!(
        kinds,
        [
            WalletKind::Key(ChainFamily::Solana),
            WalletKind::Watch(ChainFamily::Solana)
        ]
    );
    let mut portfolio = engine.portfolio();
    let held = until(&mut portfolio, |portfolio| {
        portfolio
            .holdings
            .iter()
            .any(|holding| holding.wallet == watched)
    })
    .await;
    assert!(
        held.holdings
            .iter()
            .all(|holding| holding.asset.network().as_str() == "solana")
    );
    assert!(held.holdings.iter().any(|holding| holding.wallet == key));
}
