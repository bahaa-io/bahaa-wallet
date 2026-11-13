#![allow(clippy::unwrap_used)]

mod common;

use std::sync::{Arc, Mutex};

use bahaa_chain::{Chain, ChainError};
use bahaa_solana::SolanaChain;
use bahaa_types::{Amount, AssetId};
use common::{
    AAPLX, BERN, FRIEND, Ledger, OWNER, TOKEN_2022, USDC, address, called, native, node, serve,
    solana_on, token_account, token_account_with, usdc, wallet,
};
use serde_json::{Value, json};

const OWNER_USDC: &str = "5N3f1tj9v1vc5TUZ8S7mCAnVmjVKrfnzXWhxLaxyZAgt";
const OWNER_USDC_2022: &str = "52DxsD9Yt7q6329gb9yCZwVL5tTXrjKRh2h9Ce4BqSKb";
const OWNER_AAPLX: &str = "3s4MfAv21S88M2Lsq9bwKHgd6TnA2FcXk13vRD7FvLEj";
const OWNER_BERN: &str = "29g64q6UDASAkMchmVW688eM2oRkm5sXb7JmgVPNQR4k";
const USDC_METADATA: &str = "5x38Kp4hvdomTCnCrAny4UtMUt5rQBdB6px2K1Ui45Wq";

#[tokio::test]
async fn one_round_trip_reads_the_coin_every_known_token_and_their_mints() {
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
    assert_eq!(reads.len(), 2);
    let holdings = reads
        .iter()
        .find(|read| read[1]["dataSlice"]["length"] == 109)
        .unwrap();
    assert_eq!(holdings[0], json!([OWNER, OWNER_USDC, OWNER_USDC_2022]));
    let mints = reads
        .iter()
        .find(|read| read[1].get("dataSlice").is_none())
        .unwrap();
    assert_eq!(mints[0], json!([USDC]));
    let empty = chain
        .balances(&address(FRIEND), &[native(), usdc()])
        .await
        .unwrap();
    assert!(empty.iter().all(|balance| balance.amount.is_zero()));
    assert_eq!(empty[1].amount, Amount::parse("0", 6).unwrap());
}

#[tokio::test]
async fn a_token_account_handed_to_someone_else_holds_nothing_here() {
    let mut ledger = Ledger::default();
    ledger
        .accounts
        .insert(OWNER_USDC.to_owned(), token_account(FRIEND, USDC, 5));
    let url = node(Arc::new(Mutex::new(Vec::new())), ledger.clone()).await;
    let chain = SolanaChain::new(solana_on(&[url])).unwrap();
    let held = chain.balances(&address(OWNER), &[usdc()]).await.unwrap();
    assert!(held[0].amount.is_zero());
    ledger
        .accounts
        .insert(OWNER_USDC.to_owned(), token_account(OWNER, FRIEND, 5));
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

#[tokio::test]
async fn token_2022_balances_show_what_the_token_program_shows() {
    let mut ledger = Ledger::default();
    ledger.accounts.insert(OWNER.to_owned(), wallet(1));
    ledger.accounts.insert(
        OWNER_AAPLX.to_owned(),
        token_account_with(TOKEN_2022, OWNER, AAPLX, 187_360, &[(7, &[])]),
    );
    ledger.accounts.insert(
        OWNER_BERN.to_owned(),
        token_account_with(TOKEN_2022, OWNER, BERN, 1_428, &[(7, &[]), (2, &[0; 8])]),
    );
    let url = node(Arc::new(Mutex::new(Vec::new())), ledger).await;
    let chain = SolanaChain::new(solana_on(&[url])).unwrap();
    let assets: Vec<AssetId> = [AAPLX, BERN]
        .iter()
        .map(|mint| format!("solana/{mint}").parse().unwrap())
        .collect();
    let balances = chain.balances(&address(OWNER), &assets).await.unwrap();
    let shown: Vec<String> = balances.iter().map(|b| b.amount.to_string()).collect();
    assert_eq!(shown, ["0.00188072", "0.01428"]);
    let empty = chain.balances(&address(FRIEND), &assets).await.unwrap();
    assert!(empty.iter().all(|balance| balance.amount.is_zero()));
}

#[tokio::test]
async fn token_2022_tokens_are_named_by_the_name_inside_them() {
    let url = node(Arc::new(Mutex::new(Vec::new())), Ledger::default()).await;
    let chain = SolanaChain::new(solana_on(&[url])).unwrap();
    let coin = chain.token(&address(AAPLX)).await.unwrap();
    assert_eq!(
        (coin.symbol.as_str(), coin.name.as_str(), coin.decimals),
        ("AAPLx", "Apple xStock", 8)
    );
    assert!(matches!(
        chain.token(&address(BERN)).await,
        Err(ChainError::Invalid(_))
    ));
}
