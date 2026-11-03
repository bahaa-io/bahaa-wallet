#![allow(clippy::unwrap_used)]

mod common;

use std::sync::{Arc, Mutex};

use bahaa_config::Registry;
use bahaa_crypto::{ExposeSecret, SecretString};
use bahaa_engine::{Engine, Event, Replace, SendRequest, SentStatus, Spend, WalletKind};
use bahaa_testkit::electrum::{Ledger, Log, sender};
use bahaa_types::{Address, Amount, AssetId, BitcoinType, ChainFamily};
use common::{BITCOIN_GENESIS, Scratch, bitcoin_network, config, settled, until};

const PHRASE: &str =
    "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";
const FIRST: &str = "bc1qcr8te4kr609gcawutmrza0j4xv80jy8z306fyu";
const SECOND: &str = "bc1qnjg0jd8228aq7egyzacy8cys3knf9xvrerkf9g";
const FRIEND: &str = "bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t4";

fn address(text: &str) -> Address {
    Address::parse(ChainFamily::Bitcoin, text).unwrap()
}

async fn started() -> (Scratch, Engine, Arc<Mutex<Ledger>>) {
    let mut ledger = Ledger::new(BITCOIN_GENESIS);
    ledger.fund(&address(SECOND), 50_000, 1);
    let ledger = Arc::new(Mutex::new(ledger));
    let url = bahaa_testkit::electrum::serve(Log::default(), Arc::clone(&ledger)).await;
    let scratch = Scratch::new();
    let registry = Registry::with_networks(vec![bitcoin_network(url)]).unwrap();
    let engine = Engine::start(config(&scratch, registry, None)).unwrap();
    engine.create(SecretString::from("pw")).await.unwrap();
    (scratch, engine, ledger)
}

#[tokio::test]
async fn a_phrase_holds_bitcoin_across_its_addresses_and_sends_it() {
    let (_scratch, engine, ledger) = started().await;
    let mut events = engine.events();
    let wallet = engine
        .import(SecretString::from(PHRASE), SecretString::from(""))
        .await
        .unwrap();
    let mut portfolio = engine.portfolio();
    let held = until(&mut portfolio, |portfolio| !portfolio.holdings.is_empty()).await;
    assert_eq!(held.holdings[0].address.to_string(), FIRST);
    assert_eq!(held.holdings[0].amount.to_string(), "0.0005");
    let arrived = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            if let Ok(Event::Received(arrivals)) = events.recv().await {
                return arrivals;
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(arrived[0].received.from, sender(1));
    assert_eq!(arrived[0].received.to, address(FIRST));
    assert_eq!(arrived[0].received.amount.to_string(), "0.0005");
    let request = SendRequest {
        wallet,
        account: 0,
        asset: "bitcoin".parse::<AssetId>().unwrap(),
        to: address(FRIEND),
        amount: Spend::Exact(Amount::parse("0.0002", 8).unwrap()),
        memo: None,
    };
    let draft = engine.prepare_send(request).await.unwrap();
    assert_eq!(draft.fee.expected().to_string(), "0.00000282");
    let sent = engine
        .confirm_send(draft.id, SecretString::from("pw"))
        .await
        .unwrap();
    assert_eq!(sent.from.to_string(), FIRST);
    assert_eq!(ledger.lock().unwrap().broadcast.len(), 1);
    ledger
        .lock()
        .unwrap()
        .confirm(&sent.id, &address(SECOND), 900_001);
    let done = settled(&mut events, &sent.id).await;
    assert_eq!(done.status, SentStatus::Confirmed);
    assert_eq!(done.block, Some(900_001));
    assert_eq!(done.fee_paid, Some(Amount::parse("0.00000282", 8).unwrap()));
    engine.lock().await.unwrap();
    engine.unlock(SecretString::from("pw")).await.unwrap();
    let mut portfolio = engine.portfolio();
    let again = until(&mut portfolio, |portfolio| !portfolio.holdings.is_empty()).await;
    assert_eq!(again.holdings[0].amount.to_string(), "0.0005");
}

#[tokio::test]
async fn a_bitcoin_key_and_a_watched_address_hold_one_address_each() {
    let (_scratch, engine, _) = started().await;
    let wif = bahaa_crypto::bitcoin_wif(&[0x9d; 32]);
    let key = engine
        .import_key(SecretString::from(wif.to_string()), ChainFamily::Bitcoin)
        .await
        .unwrap();
    let watched = engine.watch(address(SECOND), true).await.unwrap();
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
            WalletKind::Key(ChainFamily::Bitcoin),
            WalletKind::Watch(ChainFamily::Bitcoin)
        ]
    );
    let mut portfolio = engine.portfolio();
    let held = until(&mut portfolio, |portfolio| {
        let has = |wallet| {
            portfolio
                .holdings
                .iter()
                .any(|holding| holding.wallet == wallet)
        };
        has(watched) && has(key)
    })
    .await;
    let watched_held = held
        .holdings
        .iter()
        .find(|holding| holding.wallet == watched)
        .unwrap();
    assert_eq!(watched_held.amount.to_string(), "0.0005");
    assert_eq!(
        engine
            .reveal(key, SecretString::from("pw"))
            .await
            .unwrap()
            .expose_secret(),
        wif.as_str()
    );
}

#[tokio::test]
async fn a_phrase_used_with_taproot_is_found_there_and_remembered() {
    const TAPROOT_FIRST: &str = "bc1p5cyxnuxmeuwuvkwfem96lqzszd02n6xdcjrs20cac6yqjjwudpxqkedrcr";
    let mut ledger = Ledger::new(BITCOIN_GENESIS);
    ledger.fund(&address(TAPROOT_FIRST), 40_000, 2);
    let ledger = Arc::new(Mutex::new(ledger));
    let url = bahaa_testkit::electrum::serve(Log::default(), Arc::clone(&ledger)).await;
    let scratch = Scratch::new();
    let registry = Registry::with_networks(vec![bitcoin_network(url)]).unwrap();
    let engine = Engine::start(config(&scratch, registry, None)).unwrap();
    engine.create(SecretString::from("pw")).await.unwrap();
    let wallet = engine
        .import(SecretString::from(PHRASE), SecretString::from(""))
        .await
        .unwrap();
    let mut portfolio = engine.portfolio();
    let held = until(&mut portfolio, |portfolio| {
        portfolio
            .holdings
            .iter()
            .any(|holding| holding.address.to_string() == TAPROOT_FIRST)
    })
    .await;
    assert_eq!(held.holdings[0].amount.to_string(), "0.0004");
    let kind = |wallets: Vec<bahaa_engine::WalletInfo>| {
        wallets
            .into_iter()
            .find(|info| info.id == wallet)
            .unwrap()
            .bitcoin
    };
    assert_eq!(
        kind(engine.wallets().await.unwrap()),
        Some(BitcoinType::Taproot)
    );
    engine.lock().await.unwrap();
    engine.unlock(SecretString::from("pw")).await.unwrap();
    assert_eq!(
        kind(engine.wallets().await.unwrap()),
        Some(BitcoinType::Taproot)
    );
}

#[tokio::test]
async fn a_speed_up_replaces_the_pending_send_and_retires_it_once_confirmed() {
    let (_scratch, engine, ledger) = started().await;
    let mut events = engine.events();
    let wallet = engine
        .import(SecretString::from(PHRASE), SecretString::from(""))
        .await
        .unwrap();
    let mut portfolio = engine.portfolio();
    until(&mut portfolio, |portfolio| !portfolio.holdings.is_empty()).await;
    let request = SendRequest {
        wallet,
        account: 0,
        asset: "bitcoin".parse::<AssetId>().unwrap(),
        to: address(FRIEND),
        amount: Spend::Exact(Amount::parse("0.0002", 8).unwrap()),
        memo: None,
    };
    let draft = engine.prepare_send(request).await.unwrap();
    let first = engine
        .confirm_send(draft.id, SecretString::from("pw"))
        .await
        .unwrap();
    let faster = engine
        .replace(
            "bitcoin".parse().unwrap(),
            first.id.clone(),
            Replace::SpeedUp,
        )
        .await
        .unwrap();
    assert_eq!(faster.replaces.as_deref(), Some(first.id.as_str()));
    assert_eq!(faster.fee.expected().to_string(), "0.00000423");
    let second = engine
        .confirm_send(faster.id, SecretString::from("pw"))
        .await
        .unwrap();
    ledger
        .lock()
        .unwrap()
        .confirm(&second.id, &address(SECOND), 900_001);
    let done = settled(&mut events, &second.id).await;
    assert_eq!(done.status, SentStatus::Confirmed);
    let history = engine.history().await.unwrap();
    let retired = history.iter().find(|tx| tx.id == first.id).unwrap();
    assert_eq!(retired.status, SentStatus::Dropped);
}
