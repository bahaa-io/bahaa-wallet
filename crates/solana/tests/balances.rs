#![allow(clippy::unwrap_used)]

mod common;

use std::sync::{Arc, Mutex};

use bahaa_chain::{Chain, ChainError};
use bahaa_solana::SolanaChain;
use bahaa_types::Amount;
use common::{
    FRIEND, Ledger, OWNER, USDC, address, called, native, node, serve, solana_on, token_account,
    usdc, wallet,
};
use serde_json::{Value, json};

const OWNER_USDC: &str = "5N3f1tj9v1vc5TUZ8S7mCAnVmjVKrfnzXWhxLaxyZAgt";
const USDC_METADATA: &str = "5x38Kp4hvdomTCnCrAny4UtMUt5rQBdB6px2K1Ui45Wq";

#[tokio::test]
async fn one_read_gives_the_coin_and_every_known_token() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let mut ledger = Ledger::default();
    ledger
        .accounts
        .insert(OWNER.to_owned(), wallet(1_250_000_000));
    ledger
        .accounts
        .insert(OWNER_USDC.to_owned(), token_account(OWNER, USDC, 8_509_335));
    let chain = SolanaChain::new(solana_on(&[node(Arc::clone(&log), ledger).await])).unwrap();
    let balances = chain
        .balances(&address(OWNER), &[native(), usdc()])
        .await
        .unwrap();
    let shown: Vec<String> = balances.iter().map(|b| b.amount.to_string()).collect();
    assert_eq!(shown, ["1.25", "8.509335"]);
    assert_eq!(balances[1].asset, usdc());
    let reads = called(&log, "getMultipleAccounts");
    assert_eq!(reads.len(), 1);
    assert_eq!(reads[0][0], json!([OWNER, OWNER_USDC]));
    assert_eq!(reads[0][1]["dataSlice"]["length"], 109);
    let empty = chain
        .balances(&address(FRIEND), &[native(), usdc()])
        .await
        .unwrap();
    assert!(empty.iter().all(|balance| balance.amount.is_zero()));
    assert_eq!(empty[1].amount, Amount::parse("0", 6).unwrap());
}

#[tokio::test]
async fn a_token_account_that_is_not_ours_is_a_bad_reply() {
    let mut ledger = Ledger::default();
    ledger
        .accounts
        .insert(OWNER_USDC.to_owned(), token_account(FRIEND, USDC, 5));
    let url = node(Arc::new(Mutex::new(Vec::new())), ledger).await;
    let chain = SolanaChain::new(solana_on(&[url])).unwrap();
    assert!(matches!(
        chain.balances(&address(OWNER), &[usdc()]).await,
        Err(ChainError::Network(_))
    ));
}

#[tokio::test]
async fn a_server_on_another_cluster_is_never_used() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let devnet = serve(Arc::clone(&log), |method, _| {
        assert_eq!(method, "getGenesisHash");
        (
            200,
            json!({"jsonrpc": "2.0", "id": 1, "result": "EtWTRABZaYq6iMfeYKouRu166VU2xqa1wcaWoxPkrZBG"}),
        )
    })
    .await;
    let chain = SolanaChain::new(solana_on(&[devnet])).unwrap();
    assert!(chain.balances(&address(OWNER), &[native()]).await.is_err());
    assert!(
        log.lock()
            .unwrap()
            .iter()
            .all(|(method, _)| method == "getGenesisHash")
    );
}

#[tokio::test]
async fn custom_tokens_are_named_by_their_metadata_record() {
    let recorded: Value = serde_json::from_str(include_str!("fixtures/usdc.json")).unwrap();
    let mut ledger = Ledger::default();
    ledger
        .accounts
        .insert(USDC.to_owned(), recorded["mint"].clone());
    ledger
        .accounts
        .insert(USDC_METADATA.to_owned(), recorded["metadata"].clone());
    let url = node(Arc::new(Mutex::new(Vec::new())), ledger).await;
    let chain = SolanaChain::new(solana_on(&[url])).unwrap();
    let coin = chain.token(&address(USDC)).await.unwrap();
    assert_eq!(
        (coin.symbol.as_str(), coin.name.as_str(), coin.decimals),
        ("USDC", "USD Coin", 6)
    );
    assert_eq!(coin.price, None);
    assert!(matches!(
        chain.token(&address(FRIEND)).await,
        Err(ChainError::Invalid(_))
    ));
}
