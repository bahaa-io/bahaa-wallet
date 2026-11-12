#![allow(clippy::unwrap_used)]

mod common;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use bahaa_chain::{Chain, ChainError, FeeItem, Found, Transfer, check_signed};
use bahaa_solana::SolanaChain;
use bahaa_types::{Lock, NftKind, Trust};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use common::{
    FRIEND, Ledger, OWNER, RECORD_RENT, TOKEN_RENT, address, called, node, seed, solana_on,
    token_account, wallet,
};
use serde_json::{Value, json};

const FIXTURE: &str = include_str!("fixtures/nfts.json");
const HOLDER: &str = "BDw9cp6JDBjeih6HsFwFjj2hyHb2dtkAuu6kmUGzt8v2";
const MAD_LAD: &str = "13oLhSJnADiB6demF11T6m2MCRgxqxBjB8yJEc5WsmZ";
const MAD_LAD_ACCOUNT: &str = "QgLfZMEAYR8HRPbh4oHBr2pPW5n9mveyXJ6tdMW3Mfx";
const MAD_LAD_STATE: &str = "8MH5hns5hyAK1SWWzRCxwqHiL8t1vhGNB69m2wjWHxxF";
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
                Some(Lock::Frozen),
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
}

#[tokio::test]
async fn programmable_nfts_are_locked_while_staked_or_listed_for_sale() {
    let lock_with = |edit: &dyn Fn(&mut Ledger)| {
        let mut ledger = ledger();
        edit(&mut ledger);
        async move { listed(ledger).await.items[0].lock }
    };
    assert_eq!(lock_with(&|_| {}).await, Some(Lock::Frozen));
    let free = |ledger: &mut Ledger| patched(ledger, MAD_LAD_STATE, |data| data[2] = 0);
    assert_eq!(lock_with(&free).await, None);
    let listed_state = |ledger: &mut Ledger| patched(ledger, MAD_LAD_STATE, |data| data[2] = 2);
    assert_eq!(lock_with(&listed_state).await, Some(Lock::Listed));
    let sale = |ledger: &mut Ledger| {
        patched(ledger, MAD_LAD_STATE, |data| {
            data[2] = 0;
            data[46] = 0;
        });
    };
    assert_eq!(lock_with(&sale).await, Some(Lock::Listed));
    let missing = |ledger: &mut Ledger| {
        ledger.accounts.remove(MAD_LAD_STATE);
    };
    assert_eq!(lock_with(&missing).await, Some(Lock::Frozen));
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

const CORE: &str = "CoREENxT6tW1HoK8ypY1SxRMZTcVPm7R94rH4PZNhX7d";
const OWN_GNOME: &str = "3jmyDTM1pKwa1Earz7babpXszt8NfYW7xnVJDnDi83P4";
const OWN_MAD_LAD: &str = "GoqxN61TdTdniDnXf35s6wgtCKfJTpG97uZMoR8ksZGp";
const OWN_MAD_LAD_STATE: &str = "DUnxPW2WfrCrihVQz1TMnnCiuCSpHqRLECDMv26AqLTr";
const FRIEND_MAD_LAD: &str = "AF1Vyo3TvP89FLEvKqC5Nxo2X6WFTH51cF2rFoTLS8vK";
const FRIEND_MAD_LAD_STATE: &str = "3cbZXkfGxxjTPLFBFyfMAD2eJyYbYJGgaHxpvRLbYvRQ";
const MAD_LAD_EDITION: &str = "BQjvjGN4oX5JSfQtNnmmrnrNimEGyRrMXtdGZPV8pVWU";
const METADATA: &str = "metaqbxxUerdq28cj1RbAWkYQm3ybzjb6a8bt518x1s";
const RULES: &str = "auth9SigNpDKz4sJJ1DfCTuZrZNSAgh9sFD3rboVmgg";
const RULE_SET: &str = "eBJLFYPxJmMGKuFwpDWkzxZeUrad92kZRC5BJLpzyT9";
const INSTRUCTIONS: &str = "Sysvar1nstructions1111111111111111111111111";
const STATE_RENT: u64 = 1_447_680;

fn key(text: &str) -> [u8; 32] {
    let bahaa_types::Address::Solana(key) = address(text) else {
        unreachable!()
    };
    key
}

fn token_state(state: u8) -> Value {
    let mut data = vec![11, 255, state];
    data.resize(80, 0);
    json!({"lamports": STATE_RENT, "owner": METADATA, "executable": false, "data": [STANDARD.encode(data), "base64"], "space": 80})
}

fn sender() -> Ledger {
    let mut ledger = ledger();
    ledger.fees = vec![0, 1_000];
    ledger.units = 30_000;
    ledger
        .accounts
        .insert(OWNER.to_owned(), wallet(1_000_000_000));
    ledger
        .accounts
        .insert(FRIEND.to_owned(), wallet(500_000_000));
    for (account, mint) in [(OWN_GNOME, GNOME), (OWN_MAD_LAD, MAD_LAD)] {
        ledger
            .accounts
            .insert(account.to_owned(), token_account(OWNER, mint, 1));
    }
    patched(&mut ledger, PATRIOT, |data| {
        data[1..33].copy_from_slice(&key(OWNER));
    });
    ledger
        .accounts
        .insert(OWN_MAD_LAD_STATE.to_owned(), token_state(0));
    ledger
}

fn sent(asset: &str) -> Transfer {
    Transfer {
        account: 0,
        from: address(OWNER),
        to: address(FRIEND),
        asset: format!("solana/nft/{asset}").parse().unwrap(),
        amount: bahaa_types::Amount::parse("1", 0).unwrap(),
        max: false,
        memo: Some("gift".to_owned()),
    }
}

async fn sending(ledger: Ledger) -> SolanaChain {
    let url = node(Arc::new(Mutex::new(Vec::new())), ledger).await;
    SolanaChain::new(solana_on(&[url])).unwrap()
}

#[tokio::test]
async fn a_classic_nft_moves_like_a_one_of_one_token_and_reads_back_as_the_nft() {
    let chain = sending(sender()).await;
    let (plan, fee) = chain.prepare(&sent(GNOME)).await.unwrap();
    assert!(
        fee.lines()
            .iter()
            .any(|line| line.item == FeeItem::Activation)
    );
    let signed = chain.sign(&plan, &seed()).unwrap();
    let decoded = chain.decode(&signed).unwrap();
    check_signed(chain.planned(&plan), &decoded).unwrap();
    assert_eq!(decoded.asset, sent(GNOME).asset);
    assert_eq!(
        (decoded.amount.to_string(), decoded.memo.as_deref()),
        ("1".to_owned(), Some("gift"))
    );
    let mut max = sent(GNOME);
    max.max = true;
    let (plan, _) = chain.prepare(&max).await.unwrap();
    assert_eq!(chain.planned(&plan).amount.to_string(), "1");
}

#[tokio::test]
async fn a_core_nft_moves_through_the_core_program_with_its_collection() {
    let chain = sending(sender()).await;
    let (plan, fee) = chain.prepare(&sent(PATRIOT)).await.unwrap();
    assert!(
        !fee.lines()
            .iter()
            .any(|line| line.item == FeeItem::Activation)
    );
    let signed = chain.sign(&plan, &seed()).unwrap();
    let decoded = chain.decode(&signed).unwrap();
    check_signed(chain.planned(&plan), &decoded).unwrap();
    assert_eq!(decoded.asset, sent(PATRIOT).asset);
    for needed in [CORE, PATRIOT, PATRIOTS, FRIEND] {
        let bytes = key(needed);
        assert!(
            signed.bytes.windows(32).any(|window| window == bytes),
            "{needed} missing"
        );
    }
}

#[tokio::test]
async fn a_programmable_nft_moves_through_token_metadata_under_its_rule_set() {
    let chain = sending(sender()).await;
    let (plan, fee) = chain.prepare(&sent(MAD_LAD)).await.unwrap();
    let opening = fee
        .lines()
        .iter()
        .find(|line| line.item == FeeItem::Activation)
        .unwrap();
    assert_eq!(
        opening.amount.units(),
        ruint::aliases::U256::from(TOKEN_RENT + RECORD_RENT - STATE_RENT)
    );
    let signed = chain.sign(&plan, &seed()).unwrap();
    let decoded = chain.decode(&signed).unwrap();
    check_signed(chain.planned(&plan), &decoded).unwrap();
    assert_eq!(decoded.asset, sent(MAD_LAD).asset);
    assert_eq!(
        (decoded.amount.to_string(), decoded.memo.as_deref()),
        ("1".to_owned(), Some("gift"))
    );
    for needed in [
        METADATA,
        RULES,
        RULE_SET,
        INSTRUCTIONS,
        MAD_LAD_EDITION,
        OWN_MAD_LAD,
        OWN_MAD_LAD_STATE,
        FRIEND_MAD_LAD,
        FRIEND_MAD_LAD_STATE,
    ] {
        let bytes = key(needed);
        assert!(
            signed.bytes.windows(32).any(|window| window == bytes),
            "{needed} missing"
        );
    }
    let mut max = sent(MAD_LAD);
    max.max = true;
    let (plan, _) = chain.prepare(&max).await.unwrap();
    assert_eq!(chain.planned(&plan).amount.to_string(), "1");
    let mut opened = sender();
    opened
        .accounts
        .insert(FRIEND_MAD_LAD.to_owned(), token_account(FRIEND, MAD_LAD, 0));
    let (_, fee) = sending(opened).await.prepare(&sent(MAD_LAD)).await.unwrap();
    assert!(
        !fee.lines()
            .iter()
            .any(|line| line.item == FeeItem::Activation)
    );
}

#[tokio::test]
async fn only_a_held_movable_nft_is_sent_and_only_one_at_a_time() {
    let chain = sending(sender()).await;
    let mut two = sent(GNOME);
    two.amount = bahaa_types::Amount::parse("2", 0).unwrap();
    assert_eq!(
        chain.prepare(&two).await.err(),
        Some(ChainError::Invalid(
            "a Solana NFT is one of a kind, send just 1"
        ))
    );
    let mut stranger = sender();
    patched(&mut stranger, PATRIOT, |data| data[1..33].fill(7));
    stranger.accounts.remove(OWN_GNOME);
    let chain = sending(stranger).await;
    for asset in [PATRIOT, GNOME] {
        assert_eq!(
            chain.prepare(&sent(asset)).await.err(),
            Some(ChainError::Invalid("this wallet doesn't hold that NFT"))
        );
    }
    let mut frozen = sender();
    patched(&mut frozen, OWN_GNOME, |data| data[108] = 2);
    let chain = sending(frozen).await;
    assert_eq!(
        chain.prepare(&sent(GNOME)).await.err(),
        Some(ChainError::Invalid("this NFT is frozen and can't be sent"))
    );
    let locked =
        ChainError::Invalid("this NFT is locked, for example while staked, and can't be sent");
    let listed = ChainError::Invalid("this NFT is listed for sale, cancel the listing to send it");
    for (state, refusal) in [
        (Some(1), &locked),
        (Some(2), &listed),
        (Some(7), &locked),
        (None, &locked),
    ] {
        let mut held = sender();
        match state {
            Some(state) => {
                held.accounts
                    .insert(OWN_MAD_LAD_STATE.to_owned(), token_state(state));
            }
            None => {
                held.accounts.remove(OWN_MAD_LAD_STATE);
            }
        }
        assert_eq!(
            sending(held)
                .await
                .prepare(&sent(MAD_LAD))
                .await
                .err()
                .as_ref(),
            Some(refusal),
            "{state:?}"
        );
    }
}
