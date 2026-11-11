#![allow(clippy::unwrap_used)]

mod common;

use bahaa_chain::{Chain, FeeItem, Transfer};
use bahaa_config::Registry;
use bahaa_solana::SolanaChain;
use bahaa_types::Amount;
use common::{FRIEND, address, native, usdc};

const RICH: &str = "9WzDXwBbmkg8ZTbNMqUxvQRAyrZzDsGYdLVL9zYtAWWM";

fn mainnet() -> SolanaChain {
    let registry = Registry::builtin().unwrap();
    SolanaChain::new(registry.get(&"solana".parse().unwrap()).unwrap().clone()).unwrap()
}

#[tokio::test]
#[ignore = "talks to the real Solana mainnet"]
async fn live_balances_and_a_quote_from_a_funded_address() {
    let chain = mainnet();
    let balances = chain
        .balances(&address(RICH), &[native(), usdc()])
        .await
        .unwrap();
    assert!(!balances[0].amount.is_zero());
    let transfer = Transfer {
        account: 0,
        from: address(RICH),
        to: address(FRIEND),
        asset: native(),
        amount: Amount::parse("0.001", 9).unwrap(),
        max: false,
        memo: Some("live quote".to_owned()),
    };
    let (plan, fee) = chain.prepare(&transfer).await.unwrap();
    assert_eq!(chain.planned(&plan).amount, transfer.amount);
    assert_eq!(fee.lines()[0].item, FeeItem::Signature);
    assert!(matches!(fee.lines()[1].item, FeeItem::Priority { units } if units > 0));
}

#[tokio::test]
#[ignore = "talks to the real Solana mainnet"]
async fn live_history_reads_from_the_server_that_keeps_it() {
    let chain = mainnet();
    let scanned = chain
        .scan(&address(FRIEND), &[native(), usdc()], None)
        .await
        .unwrap()
        .unwrap();
    assert!(scanned.cursor.next > 1_790_000_000);
    for moved in scanned.found.iter().chain(&scanned.sent) {
        assert!(moved.from == address(FRIEND) || moved.to == address(FRIEND));
    }
}

#[tokio::test]
#[ignore = "talks to the real Solana network"]
async fn live_nfts_are_listed_by_das_and_checked_on_chain() {
    let chain = mainnet();
    let holder = address("6ojDZ4LrTn9E4PgF5NVLjy6NLztXPSDmwQ5jKbfUsLWU");
    let found = chain.nfts(&holder).await.unwrap();
    assert!(!found.capped);
    let mad_lads = found
        .items
        .iter()
        .filter(|item| {
            item.collection
                .as_ref()
                .and_then(|collection| collection.name.as_deref())
                == Some("Mad Lads")
        })
        .count();
    assert!(mad_lads >= 1, "{found:?}");
    assert!(
        found
            .items
            .iter()
            .all(|item| item.kind != bahaa_types::NftKind::SolanaCompressed)
    );
}
