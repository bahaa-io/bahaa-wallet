#![allow(clippy::unwrap_used)]

mod common;

use std::sync::{Arc, Mutex};

use bahaa_bitcoin::BitcoinChain;
use bahaa_chain::{Chain, ChainError, Transfer, TxStatus};
use bahaa_config::Protocol;
use bahaa_testkit::electrum::{Ledger, Log, called, sender};
use bahaa_types::Amount;
use common::{FIRST, FRIEND, GENESIS, SECOND, address, bitcoin_with, native, seed};
use url::Url;

const DEAD: &str = "tcp://127.0.0.1:9";

async fn esplora(ledger: Ledger) -> (Url, Log, Arc<Mutex<Ledger>>) {
    let log = Log::default();
    let ledger = Arc::new(Mutex::new(ledger));
    let url = bahaa_testkit::electrum::serve_esplora(Arc::clone(&log), Arc::clone(&ledger)).await;
    (url, log, ledger)
}

fn funded() -> Ledger {
    let mut ledger = Ledger::new(GENESIS);
    ledger.fund(&address(SECOND), 30_000, 1);
    ledger
}

fn transfer() -> Transfer {
    Transfer {
        account: 0,
        from: address(FIRST),
        to: address(FRIEND),
        asset: native(),
        amount: Amount::parse("0.00025", 8).unwrap(),
        max: false,
        memo: None,
    }
}

fn chain(electrum: &str, esplora: &Url) -> BitcoinChain {
    let mut network = bitcoin_with(&[(Protocol::Esplora, esplora.clone())]);
    network.endpoints[0].url = Url::parse(electrum).unwrap();
    let chain = BitcoinChain::new(network).unwrap();
    chain.derive(&seed(), 0).unwrap();
    chain
}

#[tokio::test]
async fn without_electrum_every_read_and_the_send_go_through_esplora() {
    let (url, log, ledger) = esplora(funded()).await;
    let chain = chain(DEAD, &url);
    let held = chain.balances(&address(FIRST), &[native()]).await.unwrap();
    assert_eq!(held[0].amount.to_string(), "0.0003");
    let (plan, fee) = chain.prepare(&transfer()).await.unwrap();
    assert_eq!(fee.expected().to_string(), "0.00000282");
    let signed = chain.sign(&plan, &seed()).unwrap();
    assert_eq!(chain.broadcast(&signed).await.unwrap(), signed.id);
    ledger
        .lock()
        .unwrap()
        .confirm(signed.id.as_str(), &address(SECOND), 900_005);
    let TxStatus::Confirmed { block, fee, .. } = chain
        .status(&signed.id, &address(FIRST), None)
        .await
        .unwrap()
    else {
        panic!("not confirmed");
    };
    assert_eq!(
        (block, fee.unwrap().to_string()),
        (Some(900_005), "0.00000282".to_owned())
    );
    let scanned = chain
        .scan(&address(FIRST), &[native()], None)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(scanned.found[0].from, sender(1));
    assert_eq!(scanned.sent[0].to, address(FRIEND));
    assert_eq!(called(&log, "/fee-estimates").len(), 1);
}

#[tokio::test]
async fn esplora_waits_while_electrum_answers() {
    let (esplora_url, esplora_log, _) = esplora(funded()).await;
    let electrum_ledger = Arc::new(Mutex::new(funded()));
    let electrum_url =
        bahaa_testkit::electrum::serve(Log::default(), Arc::clone(&electrum_ledger)).await;
    let chain = chain(electrum_url.as_str(), &esplora_url);
    chain.balances(&address(FIRST), &[native()]).await.unwrap();
    chain.prepare(&transfer()).await.unwrap();
    assert!(esplora_log.lock().unwrap().is_empty());
}

#[tokio::test]
async fn an_esplora_server_on_another_network_is_never_used() {
    let mut ledger = funded();
    ledger.genesis = "00".repeat(32);
    let (url, _, _) = esplora(ledger).await;
    let failed = chain(DEAD, &url)
        .balances(&address(FIRST), &[native()])
        .await
        .unwrap_err();
    assert!(failed.to_string().contains("another network"), "{failed}");
}

#[tokio::test]
async fn a_refusal_from_esplora_is_the_network_refusing() {
    let (url, _, ledger) = esplora(funded()).await;
    let chain = chain(DEAD, &url);
    let (plan, _) = chain.prepare(&transfer()).await.unwrap();
    let signed = chain.sign(&plan, &seed()).unwrap();
    ledger.lock().unwrap().refusal =
        Some("sendrawtransaction RPC error -26: min relay fee not met".to_owned());
    assert_eq!(
        chain.broadcast(&signed).await.unwrap_err(),
        ChainError::Rejected("sendrawtransaction RPC error -26: min relay fee not met".to_owned())
    );
}
