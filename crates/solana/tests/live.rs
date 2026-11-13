#![allow(clippy::unwrap_used)]

mod common;

use bahaa_chain::{Chain, FeeItem, Transfer};
use bahaa_config::Registry;
use bahaa_solana::SolanaChain;
use bahaa_types::Amount;
use common::{AAPLX, BERN, FRIEND, address, native, usdc};

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
}

#[tokio::test]
#[ignore = "talks to the real Solana network"]
async fn live_compressed_nfts_are_proven_against_their_trees() {
    let chain = mainnet();
    let found = chain
        .nfts(&address("BDw9cp6JDBjeih6HsFwFjj2hyHb2dtkAuu6kmUGzt8v2"))
        .await
        .unwrap();
    let loot = found
        .items
        .iter()
        .find(|item| {
            item.asset.nft().unwrap().address().to_string()
                == "55H5DAZa1zkYyooT8iXVSnA4EoRUDuW1PmqmFdru2Bt"
        })
        .unwrap();
    assert_eq!(loot.kind, bahaa_types::NftKind::SolanaCompressed);
    assert_eq!(
        loot.collection
            .as_ref()
            .and_then(|collection| collection.name.as_deref()),
        Some("Portals Loot Boxes")
    );
}

#[tokio::test]
#[ignore = "talks to the real Solana network"]
async fn live_nft_transfers_of_every_kind_pass_a_real_simulation() {
    let chain = mainnet();
    for asset in [
        "CFZXxEdkoA2zwCNSpc4bLgfgEEGwYpK6eem97LCvm9KD",
        "2jJT5adTaLXi5oeUCv9zsSXXYk8MEAiqLTzwPnAvu4W6",
        "61QjtYgjuuz5AeKuCGNpJ9CrNM8Q1vFyH6xcb7VB8Qw",
        "55H5DAZa1zkYyooT8iXVSnA4EoRUDuW1PmqmFdru2Bt",
        "GbjpSDm2kX1nfnuX3CKTB5G9Rc8ihaY6zkmMd2RgtFHt",
    ] {
        let transfer = Transfer {
            account: 0,
            from: address("BDw9cp6JDBjeih6HsFwFjj2hyHb2dtkAuu6kmUGzt8v2"),
            to: address(FRIEND),
            asset: format!("solana/nft/{asset}").parse().unwrap(),
            amount: Amount::parse("1", 0).unwrap(),
            max: false,
            memo: None,
        };
        let (plan, fee) = chain.prepare(&transfer).await.unwrap();
        assert_eq!(chain.planned(&plan).asset, transfer.asset);
        assert!(fee.expected().units() > ruint::aliases::U256::ZERO);
    }
}

#[tokio::test]
#[ignore = "talks to the real Solana mainnet"]
async fn live_token_2022_transfers_pass_a_real_simulation() {
    let chain = mainnet();
    for (holder, mint, amount, decimals, fee) in [
        (
            "fr6yQkDmWy6R6pecbUsxXaw6EvRJznZ2HsK5frQgud8",
            "2b1kV6DkPAnxd5ixfnxCpjxmKwqjjaYmCZfHsFu24GXo",
            "1",
            6,
            false,
        ),
        (
            "AVmoTthdrX6tKt4nDjco2D775W2YK3sDhxPcMmzUAmTY",
            AAPLX,
            "0.01",
            8,
            false,
        ),
        (
            "CbJ7xF4nZeAnhERehC4fqDtDn5EzMMZJtB9QYJotKdjB",
            BERN,
            "0.01",
            5,
            true,
        ),
    ] {
        let asset: bahaa_types::AssetId = format!("solana/{mint}").parse().unwrap();
        let held = chain
            .balances(&address(holder), std::slice::from_ref(&asset))
            .await
            .unwrap();
        assert!(!held[0].amount.is_zero(), "{mint}");
        let transfer = Transfer {
            account: 0,
            from: address(holder),
            to: address(FRIEND),
            asset,
            amount: Amount::parse(amount, decimals).unwrap(),
            max: false,
            memo: None,
        };
        let (plan, quote) = chain.prepare(&transfer).await.unwrap();
        assert_eq!(chain.planned(&plan).amount, transfer.amount, "{mint}");
        assert_eq!(quote.withheld().is_some(), fee, "{mint}");
    }
}

#[tokio::test]
#[ignore = "talks to the real Solana mainnet"]
async fn live_sns_names_follow_records_owners_and_nft_holders() {
    let chain = mainnet();
    for (name, expected) in [
        (
            "bonfida.sns",
            "Fw1ETanDZafof7xEULsnq9UY6o71Tpds89tNwPkWLb1v",
        ),
        (
            "wallet-guide-6.sns",
            "Hf4daCT4tC2Vy9RCe9q8avT68yAsNJ1dQe6xiQqyGuqZ",
        ),
        (
            "wallet-guide-4.sns",
            "Hf4daCT4tC2Vy9RCe9q8avT68yAsNJ1dQe6xiQqyGuqZ",
        ),
        (
            "sns-ip-5-wallet-1.sns",
            "ALd1XSrQMCPSRayYUoUZnp6KcP6gERfJhWzkP49CkXKs",
        ),
    ] {
        assert_eq!(
            chain
                .sns_address(name)
                .await
                .unwrap()
                .map(|address| address.to_string())
                .as_deref(),
            Some(expected),
            "{name}"
        );
    }
    assert_eq!(
        chain
            .sns_address("this-should-not-exist-4417.sns")
            .await
            .unwrap(),
        None
    );
}
