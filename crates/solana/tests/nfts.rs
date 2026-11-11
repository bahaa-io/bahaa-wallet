#![allow(clippy::unwrap_used)]

mod common;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use bahaa_chain::Found;
use bahaa_solana::SolanaChain;
use bahaa_types::{Lock, NftKind, Trust};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use common::{Ledger, address, called, node, solana_on};
use serde_json::{Value, json};

const FIXTURE: &str = include_str!("fixtures/nfts.json");
const HOLDER: &str = "BDw9cp6JDBjeih6HsFwFjj2hyHb2dtkAuu6kmUGzt8v2";
const MAD_LAD: &str = "13oLhSJnADiB6demF11T6m2MCRgxqxBjB8yJEc5WsmZ";
const MAD_LAD_ACCOUNT: &str = "QgLfZMEAYR8HRPbh4oHBr2pPW5n9mveyXJ6tdMW3Mfx";
const GNOME: &str = "2jJT5adTaLXi5oeUCv9zsSXXYk8MEAiqLTzwPnAvu4W6";
const GNOME_ACCOUNT: &str = "JAC5L1uwYoLrzjUJAdU1LHAJiiURnYaFyzXhytkQ97pQ";
const GNOME_RECORD: &str = "36asfCpqueWxeG8rtagPUvhtgkNP79i8y4Tq5NdZNzEG";
const PATRIOT: &str = "CFZXxEdkoA2zwCNSpc4bLgfgEEGwYpK6eem97LCvm9KD";
const PATRIOTS: &str = "BZb5eKtKyMr4BH3AwG7QAc9HJ7mHNEGCd3ybrWePNGUy";

fn ledger() -> Ledger {
    let fixture: Value = serde_json::from_str(FIXTURE).unwrap();
    let accounts: HashMap<String, Value> = fixture["accounts"]
        .as_object()
        .unwrap()
        .iter()
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();
    Ledger {
        accounts,
        assets: fixture["assets"]["result"].clone(),
        ..Ledger::default()
    }
}

fn patched(ledger: &mut Ledger, key: &str, edit: impl FnOnce(&mut Vec<u8>)) {
    let account = ledger.accounts.get_mut(key).unwrap();
    let mut data = STANDARD
        .decode(account["data"][0].as_str().unwrap())
        .unwrap();
    edit(&mut data);
    account["data"][0] = json!(STANDARD.encode(data));
}

async fn listed(ledger: Ledger) -> Found {
    let url = node(Arc::new(Mutex::new(Vec::new())), ledger).await;
    let chain = SolanaChain::new(solana_on(&[url])).unwrap();
    chain.nfts(&address(HOLDER)).await.unwrap()
}

fn ids(found: &Found) -> Vec<String> {
    found
        .items
        .iter()
        .map(|item| item.asset.nft().unwrap().address().to_string())
        .collect()
}

#[tokio::test]
async fn classic_programmable_and_core_nfts_are_proven_from_their_own_accounts() {
    let found = listed(ledger()).await;
    assert!(!found.capped);
    assert_eq!(ids(&found), [MAD_LAD, GNOME, PATRIOT]);
    let summary: Vec<_> = found
        .items
        .iter()
        .map(|item| {
            let collection = item.collection.as_ref().unwrap();
            (
                item.kind,
                item.name.clone().unwrap(),
                collection.address.unwrap().to_string(),
                collection.name.clone().unwrap(),
                item.trust,
                item.lock,
                item.amount.to_string(),
            )
        })
        .collect();
    assert_eq!(
        summary,
        [
            (
                NftKind::SolanaProgrammable,
                "Mad Lads #4901".to_owned(),
                "J1S9H3QjnRtBbbuD4HjPV6RpRhwuk4zKbxsnCHuTgh9w".to_owned(),
                "Mad Lads".to_owned(),
                Trust::Unknown,
                None,
                "1".to_owned()
            ),
            (
                NftKind::SolanaToken,
                "Pawnshop gnomie #3859".to_owned(),
                "6yh2j8M3y5hDYUsJ9VP4v48eRmjTZbNuYvLcqZv9FZ15".to_owned(),
                "preBanx".to_owned(),
                Trust::Unknown,
                None,
                "1".to_owned()
            ),
            (
                NftKind::SolanaCore,
                "PAT #5360".to_owned(),
                PATRIOTS.to_owned(),
                "Patriots".to_owned(),
                Trust::Unknown,
                None,
                "1".to_owned()
            ),
        ]
    );
    assert_eq!(
        found.items[0].link.as_deref(),
        Some("https://madlads.s3.us-west-2.amazonaws.com/json/4901.json")
    );
}

#[tokio::test]
async fn items_the_account_does_not_hold_alone_are_dropped() {
    let mut elsewhere = ledger();
    patched(&mut elsewhere, MAD_LAD_ACCOUNT, |data| data[32..64].fill(7));
    patched(&mut elsewhere, GNOME, |data| data[36] = 2);
    patched(&mut elsewhere, PATRIOT, |data| data[1..33].fill(7));
    assert_eq!(ids(&listed(elsewhere).await), Vec::<String>::new());
    let mut emptied = ledger();
    patched(&mut emptied, GNOME_ACCOUNT, |data| data[64..72].fill(0));
    assert_eq!(ids(&listed(emptied).await), [MAD_LAD, PATRIOT]);
}

#[tokio::test]
async fn only_verified_collections_count_and_frozen_classics_are_locked() {
    let mut ledger = ledger();
    patched(&mut ledger, GNOME_ACCOUNT, |data| data[108] = 2);
    let at = {
        let record = STANDARD
            .decode(ledger.accounts[GNOME_RECORD]["data"][0].as_str().unwrap())
            .unwrap();
        let collection = bahaa_types::Address::parse(
            bahaa_types::ChainFamily::Solana,
            "6yh2j8M3y5hDYUsJ9VP4v48eRmjTZbNuYvLcqZv9FZ15",
        )
        .unwrap();
        let bahaa_types::Address::Solana(key) = collection else {
            unreachable!()
        };
        record.windows(32).position(|window| window == key).unwrap()
    };
    patched(&mut ledger, GNOME_RECORD, |data| data[at - 1] = 0);
    ledger.accounts.remove(PATRIOTS);
    let found = listed(ledger).await;
    let gnome = &found.items[1];
    assert_eq!((gnome.lock, &gnome.collection), (Some(Lock::Frozen), &None));
    let patriot = &found.items[2];
    assert_eq!(
        (patriot.trust, &patriot.collection),
        (Trust::Flagged, &None)
    );
    assert_eq!(found.items[0].lock, None);
}

#[tokio::test]
async fn long_listings_stop_after_three_pages() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let mut ledger = ledger();
    let compressed = ledger.assets["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["compression"]["compressed"] == true)
        .unwrap()
        .clone();
    ledger.assets =
        json!({"total": 5000, "limit": 1000, "page": 1, "items": vec![compressed; 1000]});
    let url = node(Arc::clone(&log), ledger).await;
    let chain = SolanaChain::new(solana_on(&[url])).unwrap();
    let found = chain.nfts(&address(HOLDER)).await.unwrap();
    assert_eq!((found.items.len(), found.capped), (0, true));
    let pages: Vec<u64> = called(&log, "getAssetsByOwner")
        .iter()
        .map(|params| params["page"].as_u64().unwrap())
        .collect();
    assert_eq!(pages, [1, 2, 3]);
}
