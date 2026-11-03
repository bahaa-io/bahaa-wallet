#![allow(clippy::unwrap_used)]

mod common;

use std::sync::{Arc, Mutex};

use bahaa_bitcoin::BitcoinChain;
use bahaa_chain::{Chain, Transfer};
use bahaa_testkit::electrum::{Ledger, Log, called, sender, time_of};
use bahaa_types::Amount;
use common::{FIRST, FIRST_CHANGE, FRIEND, GENESIS, SECOND, address, bitcoin_on, native, seed};

async fn started(ledger: Ledger) -> (BitcoinChain, Log, Arc<Mutex<Ledger>>) {
    let log = Log::default();
    let ledger = Arc::new(Mutex::new(ledger));
    let url = bahaa_testkit::electrum::serve(Arc::clone(&log), Arc::clone(&ledger)).await;
    let chain = BitcoinChain::new(bitcoin_on(&url)).unwrap();
    chain.derive(&seed(), 0).unwrap();
    (chain, log, ledger)
}

#[tokio::test]
async fn receipts_on_any_address_of_the_account_name_their_sender() {
    let mut ledger = Ledger::new(GENESIS);
    ledger.fund(&address(SECOND), 30_000, 1);
    ledger.fund_at(&address(FIRST_CHANGE), 20_000, 2, 900_010);
    let (chain, _, _) = started(ledger).await;
    assert!(chain.can_scan());
    let scanned = chain
        .scan(&address(FIRST), &[native()], None)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(scanned.sent, []);
    let found: Vec<(String, String, String, u64, u64)> = scanned
        .found
        .iter()
        .map(|moved| {
            (
                moved.from.to_string(),
                moved.to.to_string(),
                moved.amount.to_string(),
                moved.block,
                moved.time,
            )
        })
        .collect();
    assert_eq!(
        found,
        [
            (
                sender(2).to_string(),
                FIRST.to_owned(),
                "0.0002".to_owned(),
                900_010,
                time_of(900_010)
            ),
            (
                sender(1).to_string(),
                FIRST.to_owned(),
                "0.0003".to_owned(),
                900_000,
                time_of(900_000)
            ),
        ]
    );
    assert_eq!(scanned.cursor.complete_since, time_of(900_000));
}

#[tokio::test]
async fn a_send_from_another_app_is_listed_without_its_change() {
    let mut ledger = Ledger::new(GENESIS);
    ledger.fund(&address(SECOND), 30_000, 1);
    let (chain, _, ledger) = started(ledger).await;
    let transfer = Transfer {
        account: 0,
        from: address(FIRST),
        to: address(FRIEND),
        asset: native(),
        amount: Amount::parse("0.00025", 8).unwrap(),
        max: false,
        memo: None,
    };
    let (plan, _) = chain.prepare(&transfer).await.unwrap();
    let signed = chain.sign(&plan, &seed()).unwrap();
    chain.broadcast(&signed).await.unwrap();
    ledger
        .lock()
        .unwrap()
        .confirm(signed.id.as_str(), &address(SECOND), 900_005);
    let scanned = chain
        .scan(&address(FIRST), &[native()], None)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(scanned.sent.len(), 1);
    let sent = &scanned.sent[0];
    assert_eq!(sent.id, signed.id);
    assert_eq!(sent.from, address(FIRST));
    assert_eq!(sent.to, address(FRIEND));
    assert_eq!(sent.amount.to_string(), "0.00025");
    assert_eq!(scanned.found.len(), 1);
}

#[tokio::test]
async fn later_scans_read_only_the_new_part_and_a_two_hour_margin() {
    let mut ledger = Ledger::new(GENESIS);
    ledger.fund_at(&address(SECOND), 30_000, 1, 800_000);
    ledger.fund_at(&address(SECOND), 10_000, 2, 900_000);
    let (chain, log, ledger) = started(ledger).await;
    let first = chain
        .scan(&address(FIRST), &[native()], None)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(first.found.len(), 2);
    let fetched = called(&log, "blockchain.transaction.get").len();
    ledger
        .lock()
        .unwrap()
        .fund_at(&address(SECOND), 5_000, 3, 900_200);
    let later = chain
        .scan(&address(FIRST), &[native()], Some(first.cursor))
        .await
        .unwrap()
        .unwrap();
    let amounts: Vec<String> = later
        .found
        .iter()
        .map(|moved| moved.amount.to_string())
        .collect();
    assert_eq!(amounts, ["0.00005", "0.0001"]);
    assert_eq!(
        called(&log, "blockchain.transaction.get").len(),
        fetched + 4
    );
    assert_eq!(later.cursor.complete_since, first.cursor.complete_since);
}

#[tokio::test]
async fn unconfirmed_transfers_wait_for_a_block() {
    let mut ledger = Ledger::new(GENESIS);
    ledger.fund(&address(SECOND), 30_000, 1);
    ledger
        .histories
        .values_mut()
        .for_each(|seen| seen[0]["height"] = 0.into());
    let (chain, log, _) = started(ledger).await;
    let scanned = chain
        .scan(&address(FIRST), &[native()], None)
        .await
        .unwrap()
        .unwrap();
    assert!(scanned.found.is_empty() && scanned.sent.is_empty());
    assert_eq!(called(&log, "blockchain.transaction.get").len(), 0);
}
