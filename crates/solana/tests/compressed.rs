#![allow(clippy::unwrap_used)]

mod common;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use bahaa_chain::{Chain, ChainError, FeeItem, Found, Transfer, check_signed};
use bahaa_solana::SolanaChain;
use bahaa_types::{Lock, NftKind, Trust};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use common::{FRIEND, Ledger, Log, OWNER, address, called, node, seed, solana_on, wallet};
use serde_json::{Value, json};
use sha3::{Digest, Keccak256};

const FIXTURE: &str = include_str!("fixtures/compressed.json");
const HOLDER: &str = "BDw9cp6JDBjeih6HsFwFjj2hyHb2dtkAuu6kmUGzt8v2";
const LOOT_BOX: &str = "55H5DAZa1zkYyooT8iXVSnA4EoRUDuW1PmqmFdru2Bt";
const LOOT_TREE: &str = "8xcrYR3BbaP6kB6ULThfU1RQdhNStJWa12g43oRC264K";
const LOOT_BOXES: &str = "3EJr3Lya26q5njaZaQ2ZcHkpmHPhDHt3MrGeQbCjzZYH";
const V2_ITEM: &str = "GbjpSDm2kX1nfnuX3CKTB5G9Rc8ihaY6zkmMd2RgtFHt";
const V2_TREE: &str = "BUtF5GLyfXeCuKoMBR9YqTEDqC6KaxQZxJX6cTfwBs79";
const V2_COLLECTION: &str = "4M3r74QNP86ysMDZ7KojyJ3rbVDB77oWxuJ4XYBMbeE8";
const LIMITED: &str = "P23v6wwuvRUpugULvcGQsXb5uu12WwektAakWvy4tsU";
const LIMITED_DROPS: &str = "BYnmpDzjFaEs6dghi5UsGTL5ZyoAPbwWfMaVFzDcVj8";
const UMBER: &str = "749uSQ9tDL4yHjMErr92HwRfhBUdW7oQaUbMTpKh2R97";
const UMBERS: &str = "2WRn18ZiHji4ww3bsn2RUE2DDy2V45jChCR5mZHkbt6N";
const INTH_POOL: &str = "2FiVPxvgnNpAWDw9RfLxe8xprbB94eAEFfiAs4ErmKKb";
const NOTICE: &str = "4MUfeizXHRtfXD8kCWAkYRPSmbG4VyLJfUpvsuVxuJAE";
const BUBBLEGUM: &str = "BGUMAp9Gq7iTEuizy4pqaxsTyUCBK68MDfK752saRPUY";
const SPL_NOOP: &str = "noopb9bkMVfRPU8AsbpTUg8AQkHtKwMYZiFUjNRtMmV";
const SPL_COMPRESSION: &str = "cmtDvXumGCrqC1Age74AVPhSRVXJMd8PJS91L8KbNCK";
const MPL_NOOP: &str = "mnoopTCrg4p8ry25e4bcWA9XZjbNjMTfgYVGGEdRsf3";
const MPL_COMPRESSION: &str = "mcmt6YrQEMKw8Mw43FmpRLmf7BqRnFMKmAcbxE3xkAW";

fn ledger() -> Ledger {
    let fixture: Value = serde_json::from_str(FIXTURE).unwrap();
    let mut accounts: HashMap<String, Value> = fixture["accounts"]
        .as_object()
        .unwrap()
        .iter()
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();
    let mut parts = HashMap::new();
    for (key, tree) in fixture["trees"].as_object().unwrap() {
        accounts.insert(
            key.clone(),
            json!({"lamports": tree["lamports"], "owner": tree["owner"], "executable": false, "data": [tree["head"], "base64"], "space": tree["space"]}),
        );
        parts.insert(
            (key.clone(), tree["root_at"].as_u64().unwrap()),
            tree["root"].as_str().unwrap().to_owned(),
        );
    }
    Ledger {
        accounts,
        parts,
        assets: fixture["assets"].clone(),
        proofs: fixture["proofs"].clone(),
        ..Ledger::default()
    }
}

fn key(text: &str) -> [u8; 32] {
    let bahaa_types::Address::Solana(key) = address(text) else {
        unreachable!()
    };
    key
}

fn text(key: [u8; 32]) -> String {
    bahaa_types::Address::Solana(key).to_string()
}

fn keccak(parts: &[&[u8]]) -> [u8; 32] {
    let mut hash = Keccak256::new();
    for part in parts {
        hash.update(part);
    }
    hash.finalize().into()
}

fn item<'a>(ledger: &'a mut Ledger, id: &str) -> &'a mut Value {
    ledger.assets["items"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|item| item["id"] == id)
        .unwrap()
}

fn tree_of(ledger: &Ledger, id: &str) -> (String, u64) {
    let tree = ledger.proofs[id]["tree_id"].as_str().unwrap().to_owned();
    let at = ledger.parts.keys().find(|(key, _)| *key == tree).unwrap().1;
    (tree, at)
}

fn handed_to(ledger: &mut Ledger, id: &str, owner: &str, flags: u8) {
    let entry = item(ledger, id);
    entry["ownership"]["owner"] = json!(owner);
    entry["ownership"]["delegate"] = Value::Null;
    entry["compression"]["flags"] = json!(flags);
    let entry = entry.clone();
    let zipped = &entry["compression"];
    let field = |name: &str| key(zipped[name].as_str().unwrap());
    let nonce = zipped["leaf_id"].as_u64().unwrap().to_le_bytes();
    let (data_hash, creator_hash) = (field("data_hash"), field("creator_hash"));
    let v2 = entry["interface"] == "MplBubblegumV2";
    let version = [if v2 { 2 } else { 1 }];
    let (id_key, owner_key) = (key(id), key(owner));
    let mut parts: Vec<&[u8]> = vec![
        &version,
        &id_key,
        &owner_key,
        &owner_key,
        &nonce,
        &data_hash,
        &creator_hash,
    ];
    let (collection, asset_data_hash, flags) = (
        keccak(&[&key(entry["grouping"][0]["group_value"]
            .as_str()
            .unwrap_or(BUBBLEGUM))]),
        if v2 {
            field("asset_data_hash")
        } else {
            [0; 32]
        },
        [flags],
    );
    if v2 {
        parts.extend([collection.as_slice(), &asset_data_hash, &flags]);
    }
    let leaf = keccak(&parts);
    let proof = ledger.proofs[id].clone();
    let nodes = proof["proof"].as_array().unwrap();
    let index = proof["node_index"].as_u64().unwrap() - (1 << nodes.len());
    let root = nodes
        .iter()
        .enumerate()
        .fold(leaf, |node, (depth, sibling)| {
            let sibling = key(sibling.as_str().unwrap());
            if (index >> depth) & 1 == 0 {
                keccak(&[&node, &sibling])
            } else {
                keccak(&[&sibling, &node])
            }
        });
    ledger.proofs[id]["root"] = json!(text(root));
    let place = tree_of(ledger, id);
    ledger.parts.insert(place, STANDARD.encode(root));
}

async fn listed(ledger: Ledger) -> (Found, Log) {
    let log = Arc::new(Mutex::new(Vec::new()));
    let url = node(Arc::clone(&log), ledger).await;
    let chain = SolanaChain::new(solana_on(&[url])).unwrap();
    (chain.nfts(&address(HOLDER)).await.unwrap(), log)
}

fn ids(found: &Found) -> Vec<String> {
    found
        .items
        .iter()
        .map(|item| item.asset.nft().unwrap().address().to_string())
        .collect()
}

#[tokio::test]
async fn compressed_nfts_show_once_their_leaf_reaches_the_root_on_chain() {
    let (found, log) = listed(ledger()).await;
    assert!(!found.capped);
    let summary: Vec<_> = found
        .items
        .iter()
        .map(|item| {
            (
                item.asset.nft().unwrap().address().to_string(),
                item.kind,
                item.name.clone(),
                item.collection.as_ref().map(|collection| {
                    (
                        collection.address.unwrap().to_string(),
                        collection.name.clone(),
                    )
                }),
                item.trust,
                item.lock,
            )
        })
        .collect();
    let named = |text: &str| Some(text.to_owned());
    let compressed = NftKind::SolanaCompressed;
    assert_eq!(
        summary,
        [
            (
                LOOT_BOX.to_owned(),
                compressed,
                named("Claynosaurz Loot Box"),
                Some((LOOT_BOXES.to_owned(), named("Portals Loot Boxes"))),
                Trust::Unknown,
                None
            ),
            (
                V2_ITEM.to_owned(),
                compressed,
                named("0aHYQhRkAqayXW2q3fBNtQpRur1EnK6h"),
                Some((
                    V2_COLLECTION.to_owned(),
                    named("5C9rjEtk1REisTVehoMcVmDEz46WBaYN")
                )),
                Trust::Unknown,
                None
            ),
            (
                LIMITED.to_owned(),
                compressed,
                named("Limited Drop"),
                Some((LIMITED_DROPS.to_owned(), None)),
                Trust::Unknown,
                Some(Lock::Unsupported)
            ),
            (
                UMBER.to_owned(),
                compressed,
                named("Nhishi Umber 3"),
                Some((UMBERS.to_owned(), None)),
                Trust::Unknown,
                None
            ),
            (
                INTH_POOL.to_owned(),
                compressed,
                named("Mad Lads Inth Pool #1295"),
                None,
                Trust::Unknown,
                None
            ),
            (
                NOTICE.to_owned(),
                compressed,
                None,
                None,
                Trust::Flagged,
                Some(Lock::Unsupported)
            ),
        ]
    );
    assert_eq!(
        found.items[0].link.as_deref(),
        Some(
            "https://shdw-drive.genesysgo.net/63pJNMwRHXoFJ7x4BFoa1gQiDyxHQEm7hVEg1yimv1Xy/06ea8dc0-95df-4b6b-8cf3-b41d88f7a3ef.json"
        )
    );
    assert_eq!(called(&log, "getAssetProofBatch").len(), 1);
    assert_eq!(called(&log, "getAccountInfo").len(), 6);
    assert_eq!(
        called(&log, "getAssetsByOwner")[0]["options"]["showUnverifiedCollections"],
        true
    );
}

#[tokio::test]
async fn a_listing_that_claims_more_than_the_tree_proves_is_not_believed() {
    let mut proof = ledger();
    proof.proofs[LOOT_BOX]["proof"][0] = json!(V2_COLLECTION);
    let mut tree = ledger();
    tree.accounts.get_mut(LOOT_TREE).unwrap()["owner"] = json!(BUBBLEGUM);
    let mut nonce = ledger();
    item(&mut nonce, LOOT_BOX)["compression"]["leaf_id"] = json!(1);
    let mut owner = ledger();
    item(&mut owner, LOOT_BOX)["ownership"]["owner"] = json!(FRIEND);
    let mut root = ledger();
    let place = tree_of(&root, LOOT_BOX);
    root.parts.insert(place, STANDARD.encode([7; 32]));
    let mut missing = ledger();
    missing.proofs.as_object_mut().unwrap().remove(LOOT_BOX);
    for edited in [proof, tree, nonce, owner, root, missing] {
        let (found, _) = listed(edited).await;
        assert_eq!(found.items.len(), 5);
        assert!(!ids(&found).contains(&LOOT_BOX.to_owned()));
    }
    let mut renamed = ledger();
    item(&mut renamed, LOOT_BOX)["content"]["metadata"]["name"] = json!("Mad Lads #1");
    let (found, _) = listed(renamed).await;
    let loot = &found.items[0];
    assert_eq!(
        (loot.trust, loot.name.as_deref(), loot.collection.is_none()),
        (Trust::Flagged, None, true)
    );
}

#[tokio::test]
async fn a_root_the_tree_has_moved_past_is_found_in_its_change_log() {
    let mut ledger = ledger();
    let (tree, at) = tree_of(&ledger, LOOT_BOX);
    let head = STANDARD
        .decode(ledger.accounts[&tree]["data"][0].as_str().unwrap())
        .unwrap();
    let buffer = u64::from(u32::from_le_bytes(head[2..6].try_into().unwrap()));
    let depth = u64::from(u32::from_le_bytes(head[6..10].try_into().unwrap()));
    let active = u64::from_le_bytes(head[64..72].try_into().unwrap());
    let stride = 40 + 32 * depth;
    let real = STANDARD.decode(&ledger.parts[&(tree.clone(), at)]).unwrap();
    let mut changes = vec![0; usize::try_from(buffer * stride).unwrap()];
    let earlier = usize::try_from(((active + buffer - 1) % buffer) * stride).unwrap();
    changes[earlier..earlier + 32].copy_from_slice(&real);
    ledger
        .parts
        .insert((tree.clone(), at), STANDARD.encode([7; 32]));
    ledger
        .parts
        .insert((tree.clone(), 80), STANDARD.encode(&changes));
    let (found, log) = listed(ledger).await;
    assert!(ids(&found).contains(&LOOT_BOX.to_owned()));
    let regions: Vec<Value> = called(&log, "getAccountInfo")
        .into_iter()
        .filter(|params| params[1]["dataSlice"]["offset"] == 80)
        .collect();
    assert_eq!(regions.len(), 1);
    assert_eq!(regions[0][0], tree.as_str());
}

fn sender() -> Ledger {
    let mut ledger = ledger();
    ledger.fees = vec![0, 1_000];
    ledger.units = 40_000;
    ledger
        .accounts
        .insert(OWNER.to_owned(), wallet(1_000_000_000));
    ledger
        .accounts
        .insert(FRIEND.to_owned(), wallet(500_000_000));
    for id in [LOOT_BOX, V2_ITEM, LIMITED] {
        handed_to(&mut ledger, id, OWNER, 0);
    }
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

async fn sending(ledger: Ledger) -> (SolanaChain, Log) {
    let log = Arc::new(Mutex::new(Vec::new()));
    let url = node(Arc::clone(&log), ledger).await;
    (SolanaChain::new(solana_on(&[url])).unwrap(), log)
}

#[tokio::test]
async fn a_compressed_nft_moves_through_bubblegum_with_the_proof_its_tree_needs() {
    let ledger = sender();
    let proof: Vec<String> = ledger.proofs[LOOT_BOX]["proof"]
        .as_array()
        .unwrap()
        .iter()
        .map(|node| node.as_str().unwrap().to_owned())
        .collect();
    let (chain, log) = sending(ledger).await;
    let (plan, fee) = chain.prepare(&sent(LOOT_BOX)).await.unwrap();
    let items: Vec<FeeItem> = fee.lines().iter().map(|line| line.item.clone()).collect();
    assert_eq!(
        items,
        [FeeItem::Signature, FeeItem::Priority { units: 48_000 }]
    );
    let signed = chain.sign(&plan, &seed()).unwrap();
    let decoded = chain.decode(&signed).unwrap();
    check_signed(chain.planned(&plan), &decoded).unwrap();
    assert_eq!(decoded.asset, sent(LOOT_BOX).asset);
    assert_eq!(
        (decoded.amount.to_string(), decoded.memo.as_deref()),
        ("1".to_owned(), Some("gift"))
    );
    let carried = |needed: &str| {
        let bytes = key(needed);
        signed.bytes.windows(32).any(|window| window == bytes)
    };
    for needed in [BUBBLEGUM, LOOT_TREE, SPL_NOOP, SPL_COMPRESSION, FRIEND] {
        assert!(carried(needed), "{needed} missing");
    }
    assert!(proof[..7].iter().all(|node| carried(node)));
    assert!(!proof[7..].iter().any(|node| carried(node)));
    let fees = called(&log, "getRecentPrioritizationFees");
    assert!(
        fees[0][0]
            .as_array()
            .unwrap()
            .iter()
            .any(|key| key == LOOT_TREE)
    );
    let mut max = sent(LOOT_BOX);
    max.max = true;
    let (plan, _) = chain.prepare(&max).await.unwrap();
    assert_eq!(chain.planned(&plan).amount.to_string(), "1");
}

#[tokio::test]
async fn a_version_2_nft_pays_the_program_fee_and_names_its_collection() {
    let (chain, _) = sending(sender()).await;
    let (plan, fee) = chain.prepare(&sent(V2_ITEM)).await.unwrap();
    let program = fee
        .lines()
        .iter()
        .find(|line| line.item == FeeItem::Contracts)
        .unwrap();
    assert_eq!(program.amount.to_string(), "0.000006");
    let signed = chain.sign(&plan, &seed()).unwrap();
    let decoded = chain.decode(&signed).unwrap();
    check_signed(chain.planned(&plan), &decoded).unwrap();
    assert_eq!(decoded.asset, sent(V2_ITEM).asset);
    for needed in [V2_TREE, V2_COLLECTION, MPL_NOOP, MPL_COMPRESSION] {
        let bytes = key(needed);
        assert!(
            signed.bytes.windows(32).any(|window| window == bytes),
            "{needed} missing"
        );
    }
}

#[tokio::test]
async fn only_a_held_free_compressed_nft_that_fits_is_sent() {
    let (chain, _) = sending(sender()).await;
    assert_eq!(
        chain.prepare(&sent(LIMITED)).await.err(),
        Some(ChainError::Invalid(
            "this NFT needs a bigger transaction than this wallet can send"
        ))
    );
    for asset in [UMBER, "9xQeWvG816bUx9EPjHmaT23yvVM2ZWbrrpZb9PusVFin"] {
        assert_eq!(
            chain.prepare(&sent(asset)).await.err(),
            Some(ChainError::Invalid("this wallet doesn't hold that NFT"))
        );
    }
    for (flags, refusal) in [
        (1, "this NFT is frozen and can't be sent"),
        (2, "this NFT is frozen and can't be sent"),
        (4, "this NFT is bound to its owner and can't be sent"),
    ] {
        let mut locked = sender();
        handed_to(&mut locked, V2_ITEM, OWNER, flags);
        let (chain, _) = sending(locked).await;
        assert_eq!(
            chain.prepare(&sent(V2_ITEM)).await.err(),
            Some(ChainError::Invalid(refusal)),
            "{flags}"
        );
    }
}
