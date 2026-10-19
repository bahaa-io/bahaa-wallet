#![allow(clippy::unwrap_used)]

mod common;

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bahaa_chain::ChainError;
use bahaa_config::Registry;
use bahaa_crypto::SecretString;
use bahaa_engine::{
    Engine, EngineConfig, EngineError, Event, KeyringError, SendRequest, SentStatus, SentTx, Spend,
    WalletId,
};
use bahaa_types::{Address, Amount, ChainFamily};
use common::{HARDHAT, HARDHAT_0, HARDHAT_1, Scratch, USDT, config, result};
use serde_json::{Value, json};
use sha3::{Digest, Keccak256};
use tokio::sync::broadcast;

#[derive(Default)]
struct Ledger {
    accepted: Vec<String>,
    known: BTreeSet<String>,
    hold: bool,
    garble_next: bool,
    refuse: Option<&'static str>,
}

type Shared = Arc<Mutex<Ledger>>;

fn unhex(text: &str) -> Vec<u8> {
    let digits = text.strip_prefix("0x").unwrap();
    (0..digits.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(&digits[at..at + 2], 16).unwrap())
        .collect()
}

fn hash_of(raw: &str) -> String {
    format!("0x{}", common::hex(&Keccak256::digest(unhex(raw))))
}

fn answer(ledger: &Shared, call: &Value) -> Value {
    let mut ledger = ledger.lock().unwrap();
    let first = call["params"][0].as_str().unwrap_or_default().to_owned();
    let reply = match call["method"].as_str().unwrap() {
        "eth_chainId" => json!("0x38"),
        "eth_getBalance" => json!("0x8ac7230489e80000"),
        "eth_getTransactionCount" => json!("0x0"),
        "eth_feeHistory" => {
            json!({"oldestBlock": "0x1", "baseFeePerGas": ["0x3b9aca00", "0x3b9aca00"]})
        }
        "eth_maxPriorityFeePerGas" => json!("0x3b9aca00"),
        "eth_estimateGas" => json!("0x5208"),
        "eth_call" => json!(format!("0x{:064x}", 0)),
        "eth_sendRawTransaction" => {
            if let Some(reason) = ledger.refuse {
                return common::failure(-32000, reason);
            }
            if ledger.garble_next {
                ledger.garble_next = false;
                return result(json!(format!("0x{}", "00".repeat(32))));
            }
            let hash = hash_of(&first);
            ledger.known.insert(hash.clone());
            ledger.accepted.push(first);
            json!(hash)
        }
        "eth_getTransactionReceipt" if ledger.known.contains(&first) && !ledger.hold => json!({
            "status": "0x1",
            "blockNumber": "0x10",
            "gasUsed": "0x5208",
            "effectiveGasPrice": "0x77359400"
        }),
        "eth_getTransactionByHash" if ledger.known.contains(&first) => json!({"hash": first}),
        _ => Value::Null,
    };
    result(reply)
}

async fn ledger_node(ledger: &Shared) -> Registry {
    let shared = Arc::clone(ledger);
    let url = common::node(move |call| (answer(&shared, call), Duration::ZERO)).await;
    Registry::with_networks(vec![common::network("bsc", url)]).unwrap()
}

async fn setup(
    scratch: &Scratch,
    ledger: &Shared,
    tune: impl FnOnce(&mut EngineConfig),
) -> (Engine, WalletId) {
    let mut config = config(scratch, ledger_node(ledger).await, None);
    tune(&mut config);
    let engine = Engine::start(config).unwrap();
    engine.create(SecretString::from("pw")).await.unwrap();
    let wallet = engine
        .import(SecretString::from(HARDHAT), SecretString::from(""))
        .await
        .unwrap();
    (engine, wallet)
}

fn bnb(wallet: WalletId, amount: &str) -> SendRequest {
    SendRequest {
        wallet,
        asset: "bsc".parse().unwrap(),
        to: Address::parse(ChainFamily::Evm, HARDHAT_1).unwrap(),
        amount: Spend::Exact(Amount::parse(amount, 18).unwrap()),
    }
}

async fn send(engine: &Engine, wallet: WalletId, amount: &str) -> Result<SentTx, EngineError> {
    let draft = engine.prepare_send(bnb(wallet, amount)).await?;
    engine
        .confirm_send(draft.id, SecretString::from("pw"))
        .await
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
async fn a_send_needs_the_password_and_is_tracked_until_confirmed() {
    let scratch = Scratch::new();
    let ledger = Shared::default();
    let (engine, wallet) = setup(&scratch, &ledger, |_| {}).await;
    let mut events = engine.events();
    let draft = engine.prepare_send(bnb(wallet, "0.25")).await.unwrap();
    assert_eq!(draft.transfer.from.to_string(), HARDHAT_0);
    assert_eq!(
        (
            draft.fee.max().to_string(),
            draft.fee.expected().to_string()
        ),
        ("0.000063".to_owned(), "0.000042".to_owned())
    );
    assert_eq!(
        engine
            .confirm_send(draft.id, SecretString::from("nope"))
            .await,
        Err(EngineError::Keyring(KeyringError::WrongPassword))
    );
    assert_eq!(ledger.lock().unwrap().accepted, Vec::<String>::new());
    let sent = engine
        .confirm_send(draft.id, SecretString::from("pw"))
        .await
        .unwrap();
    assert_eq!(sent.status, SentStatus::Pending);
    assert_eq!(sent.id, hash_of(&ledger.lock().unwrap().accepted[0]));
    assert_eq!(
        engine
            .confirm_send(draft.id, SecretString::from("pw"))
            .await,
        Err(EngineError::UnknownDraft)
    );
    let done = settled(&mut events, &sent.id).await;
    assert_eq!(done.status, SentStatus::Confirmed);
    assert_eq!((done.block, done.raw.as_deref()), (Some(16), None));
    assert_eq!(done.fee_paid.unwrap().to_string(), "0.000042");
    assert_eq!(done.transaction.as_deref(), Some(sent.id.as_str()));
    assert_eq!(done.amount.to_string(), "0.25");
    assert_eq!(engine.history().await.unwrap(), [done]);
    assert_eq!(ledger.lock().unwrap().accepted.len(), 1);
}

#[tokio::test]
async fn a_max_send_is_drafted_with_the_amount_it_really_sends() {
    let scratch = Scratch::new();
    let ledger = Shared::default();
    let (engine, wallet) = setup(&scratch, &ledger, |_| {}).await;
    let everything = SendRequest {
        amount: Spend::Max,
        ..bnb(wallet, "1")
    };
    let draft = engine.prepare_send(everything).await.unwrap();
    assert!(draft.transfer.max);
    assert_eq!(
        (
            draft.transfer.amount.to_string(),
            draft.fee.max().to_string()
        ),
        ("9.999937".to_owned(), "0.000063".to_owned())
    );
    let sent = engine
        .confirm_send(draft.id, SecretString::from("pw"))
        .await
        .unwrap();
    assert_eq!(sent.amount, draft.transfer.amount);
    assert_eq!(sent.fee_max, draft.fee.max());
    assert_eq!(ledger.lock().unwrap().accepted.len(), 1);
}

#[tokio::test]
async fn drafts_expire_and_do_not_survive_a_lock() {
    let scratch = Scratch::new();
    let ledger = Shared::default();
    let (engine, wallet) = setup(&scratch, &ledger, |config| {
        config.draft_ttl = Duration::from_millis(100);
    })
    .await;
    let draft = engine.prepare_send(bnb(wallet, "1")).await.unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(
        engine
            .confirm_send(draft.id, SecretString::from("pw"))
            .await,
        Err(EngineError::DraftExpired)
    );
    let draft = engine.prepare_send(bnb(wallet, "1")).await.unwrap();
    engine.lock().await.unwrap();
    assert_eq!(
        engine.prepare_send(bnb(wallet, "1")).await,
        Err(EngineError::Keyring(KeyringError::Locked))
    );
    engine.unlock(SecretString::from("pw")).await.unwrap();
    assert_eq!(
        engine
            .confirm_send(draft.id, SecretString::from("pw"))
            .await,
        Err(EngineError::UnknownDraft)
    );
    let mut to_contract = bnb(wallet, "1");
    to_contract.asset = format!("bsc/{USDT}").parse().unwrap();
    to_contract.to = Address::parse(ChainFamily::Evm, USDT).unwrap();
    assert!(matches!(
        engine.prepare_send(to_contract).await,
        Err(EngineError::Chain(ChainError::Invalid(_)))
    ));
    assert_eq!(ledger.lock().unwrap().accepted, Vec::<String>::new());
}

#[tokio::test]
async fn a_refused_send_is_reported_and_kept_as_failed() {
    let scratch = Scratch::new();
    let ledger = Shared::default();
    ledger.lock().unwrap().refuse = Some("insufficient funds for gas * price + value");
    let (engine, wallet) = setup(&scratch, &ledger, |_| {}).await;
    let refused = send(&engine, wallet, "0.5").await;
    let Err(EngineError::Chain(ChainError::Rejected(reason))) = refused else {
        panic!("expected a rejection, got {refused:?}");
    };
    assert_eq!(reason, "insufficient funds for gas * price + value");
    let history = engine.history().await.unwrap();
    assert_eq!(history[0].status, SentStatus::Failed(reason));
    assert_eq!(history[0].raw, None);
}

#[tokio::test]
async fn an_unacknowledged_send_is_kept_and_delivered_later() {
    let scratch = Scratch::new();
    let ledger = Shared::default();
    ledger.lock().unwrap().garble_next = true;
    let (engine, wallet) = setup(&scratch, &ledger, |_| {}).await;
    let mut events = engine.events();
    let sent = send(&engine, wallet, "0.1").await.unwrap();
    assert_eq!(sent.status, SentStatus::Sending);
    assert!(sent.raw.is_some());
    let done = settled(&mut events, &sent.id).await;
    assert_eq!(done.status, SentStatus::Confirmed);
    let accepted = ledger.lock().unwrap().accepted.clone();
    assert_eq!(accepted.len(), 1);
    assert_eq!(hash_of(&accepted[0]), sent.id);
}

#[tokio::test]
async fn two_sends_never_share_a_nonce() {
    let scratch = Scratch::new();
    let ledger = Shared::default();
    ledger.lock().unwrap().hold = true;
    let (engine, wallet) = setup(&scratch, &ledger, |_| {}).await;
    let first = send(&engine, wallet, "0.1").await.unwrap();
    assert_eq!(first.status, SentStatus::Pending);
    assert_eq!(
        send(&engine, wallet, "0.2").await,
        Err(EngineError::StillSending)
    );
    assert_eq!(ledger.lock().unwrap().accepted.len(), 1);
}

#[tokio::test]
async fn pending_sends_resume_after_a_restart() {
    let scratch = Scratch::new();
    let ledger = Shared::default();
    ledger.lock().unwrap().hold = true;
    let (engine, wallet) = setup(&scratch, &ledger, |_| {}).await;
    let sent = send(&engine, wallet, "0.3").await.unwrap();
    assert_eq!(sent.status, SentStatus::Pending);
    engine.shutdown().await.unwrap();
    ledger.lock().unwrap().hold = false;
    let engine = Engine::start(config(&scratch, ledger_node(&ledger).await, None)).unwrap();
    assert_eq!(
        engine.history().await,
        Err(EngineError::Keyring(KeyringError::Locked))
    );
    let mut events = engine.events();
    engine.unlock(SecretString::from("pw")).await.unwrap();
    let done = settled(&mut events, &sent.id).await;
    assert_eq!(done.status, SentStatus::Confirmed);
    assert_eq!(
        engine.history().await.unwrap()[0].status,
        SentStatus::Confirmed
    );
}
