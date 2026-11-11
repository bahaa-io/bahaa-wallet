#![allow(clippy::unwrap_used)]

mod common;

use std::sync::{Arc, Mutex};

use bahaa_chain::{Chain, ChainError, Found, Replace, Transfer, check_signed};
use bahaa_config::{Endpoint, Feature, Protocol};
use bahaa_crypto::{Mnemonic, Seed};
use bahaa_evm::EvmChain;
use bahaa_testkit::Reply;
use bahaa_types::{Address, Amount, AssetId, ChainFamily, NftKind};
use common::{Log, OWNER, network_on, node, owner, result};
use serde_json::{Value, json};
use url::Url;

const APES: &str = "0xbc4ca0eda7647a8ab7c2061c2e118a18a936f13d";
const ITEMS: &str = "0x2222222222222222222222222222222222222222";
const PLAIN: &str = "0x3333333333333333333333333333333333333333";
const BROKEN: &str = "0x4444444444444444444444444444444444444444";
const STRANGER: &str = "0x5555555555555555555555555555555555555555";
const FRIEND: &str = "0x70997970c51812dc3a010c7d01b50e0d17dc79c8";

fn seed() -> Seed {
    Mnemonic::parse("test test test test test test test test test test test junk")
        .unwrap()
        .to_seed("")
}

fn padded(address: &str) -> String {
    format!("{:0>64}", address[2..].to_ascii_lowercase())
}

fn number(value: u64) -> String {
    format!("{value:064x}")
}

fn text(value: &str) -> String {
    let mut body = bahaa_types::hex::encode(value.as_bytes());
    while !body.len().is_multiple_of(64) {
        body.push('0');
    }
    format!("0x{}{}{body}", number(32), number(value.len() as u64))
}

fn reverted() -> (u16, Value) {
    (
        200,
        json!({"jsonrpc": "2.0", "id": 1, "error": {"code": 3, "message": "execution reverted"}}),
    )
}

fn contract_call(to: &str, data: &str) -> (u16, Value) {
    let supports = |interface: &str| format!("0x01ffc9a7{interface}{}", "0".repeat(56));
    let yes = || result(&json!(format!("0x{}", number(1))));
    let no = || result(&json!(format!("0x{}", number(0))));
    let owner = padded(OWNER);
    match (to, data) {
        (APES | BROKEN, d) if d == supports("80ac58cd") => yes(),
        (APES | BROKEN, d) if d.starts_with("0x01ffc9a7") => no(),
        (APES, "0x06fdde03") => result(&json!(text("Apes"))),
        (APES, d) if d == format!("0x6352211e{}", number(1)) => {
            result(&json!(format!("0x{owner}")))
        }
        (APES, d) if d == format!("0x6352211e{}", number(2)) => {
            result(&json!(format!("0x{}", padded(STRANGER))))
        }
        (APES, d) if d == format!("0xc87b56dd{}", number(1)) => {
            result(&json!(text("ipfs://apes/1")))
        }
        (ITEMS, d) if d == supports("d9b67a26") => yes(),
        (ITEMS, d) if d.starts_with("0x01ffc9a7") => no(),
        (ITEMS, d) if d == format!("0x00fdd58e{owner}{}", number(7)) => {
            result(&json!(format!("0x{}", number(3))))
        }
        (ITEMS, d) if d == format!("0x0e89341c{}", number(7)) => {
            result(&json!(text("https://items.example/{id}.json")))
        }
        _ => reverted(),
    }
}

async fn rpc(log: Log) -> Url {
    node(log, |method, params| match method {
        "eth_chainId" => result(&json!("0x1")),
        "eth_call" => contract_call(
            &params[0]["to"].as_str().unwrap().to_ascii_lowercase(),
            params[0]["data"].as_str().unwrap(),
        ),
        "eth_estimateGas" => result(&json!("0x1d4c0")),
        "eth_feeHistory" => result(&json!({"baseFeePerGas": ["0x3b9aca00", "0x3b9aca00"]})),
        "eth_maxPriorityFeePerGas" => result(&json!("0x3b9aca00")),
        "eth_getTransactionCount" => result(&json!("0x5")),
        "eth_getBalance" => result(&json!("0xde0b6b3a7640000")),
        _ => (200, json!({"jsonrpc": "2.0", "id": 1, "error": {"code": -32601, "message": "no such method"}})),
    })
    .await
}

fn blockscout_page(items: &[(&str, u64, &str)], next: &Value) -> Value {
    let items: Vec<Value> = items
        .iter()
        .map(|(contract, id, kind)| {
            json!({"id": id.to_string(), "token_type": kind, "value": "1", "token": {"address_hash": contract, "name": "Fake Name", "reputation": "ok"}})
        })
        .collect();
    json!({"items": items, "next_page_params": next})
}

async fn blockscout(asked: Arc<Mutex<Vec<String>>>, broken: bool) -> Url {
    bahaa_testkit::serve(move |request| {
        let path = request.path().to_owned();
        asked.lock().unwrap().push(path.clone());
        if broken {
            return Reply::json(503, &json!({"message": "busy"}));
        }
        let page = if path.contains("token_id=") {
            blockscout_page(
                &[(ITEMS, 7, "ERC-1155"), (BROKEN, 9, "ERC-721"), (APES, 1, "ERC-404")],
                &Value::Null,
            )
        } else {
            blockscout_page(
                &[(APES, 1, "ERC-721"), (APES, 2, "ERC-721"), (PLAIN, 5, "ERC-721")],
                &json!({"token_type": "ERC-721", "token_contract_address_hash": PLAIN, "token_id": "5", "items_count": 50}),
            )
        };
        Reply::json(200, &page)
    })
    .await
}

async fn routescan(asked: Arc<Mutex<Vec<String>>>) -> Url {
    bahaa_testkit::serve(move |request| {
        let path = request.path().to_owned();
        asked.lock().unwrap().push(path.clone());
        let item = |contract: &str, id: u64| json!({"chainId": "1", "tokenAddress": contract, "tokenId": id.to_string()});
        let page = if path.contains("erc721-holdings") && !path.contains("next=") {
            json!({"items": [item(APES, 1)], "link": {"nextToken": "more"}})
        } else if path.contains("erc721-holdings") {
            json!({"items": [item(APES, 2)], "link": {}})
        } else {
            json!({"items": [item(ITEMS, 7)], "link": {}})
        };
        Reply::json(200, &page)
    })
    .await
}

fn listing(protocol: Protocol, url: Url) -> Endpoint {
    Endpoint {
        protocol,
        url,
        features: vec![Feature::Nfts],
        min_gap_ms: 0,
        history_gap_ms: 0,
    }
}

async fn chain(listings: Vec<Endpoint>) -> EvmChain {
    let mut network = network_on("ethereum", &[rpc(Log::default()).await]);
    network.endpoints.extend(listings);
    EvmChain::new(network).unwrap()
}

type Summary = (String, NftKind, String, Option<String>, Option<String>);

fn summary(found: &Found) -> Vec<Summary> {
    found
        .items
        .iter()
        .map(|item| {
            (
                item.asset.to_string(),
                item.kind,
                item.amount.to_string(),
                item.collection.as_ref().unwrap().name.clone(),
                item.link.clone(),
            )
        })
        .collect()
}

fn ape(id: u64) -> String {
    format!("ethereum/nft/0xBC4CA0EdA7647A8aB7C2061c2E118A18a936f13D/{id}")
}

fn item() -> String {
    format!("ethereum/nft/{ITEMS}/7")
}

#[tokio::test]
async fn listed_nfts_count_only_when_their_own_contract_confirms_them() {
    let asked = Arc::default();
    let url = blockscout(Arc::clone(&asked), false).await;
    let found = chain(vec![listing(Protocol::Blockscout, url)])
        .await
        .nfts(&owner())
        .await
        .unwrap();
    assert!(!found.capped);
    assert_eq!(
        summary(&found),
        [
            (
                ape(1),
                NftKind::Erc721,
                "1".to_owned(),
                Some("Apes".to_owned()),
                Some("ipfs://apes/1".to_owned())
            ),
            (
                item(),
                NftKind::Erc1155,
                "3".to_owned(),
                None,
                Some(format!("https://items.example/{}.json", number(7)))
            ),
        ]
    );
    let asked = asked.lock().unwrap();
    assert_eq!(asked.len(), 2);
    assert!(asked[0].contains("type=ERC-721%2CERC-1155"), "{asked:?}");
    assert!(asked[1].contains("token_id=5"), "{asked:?}");
}

#[tokio::test]
async fn routescan_steps_in_when_blockscout_fails_and_lists_both_kinds() {
    let (failed, served) = (Arc::default(), Arc::default());
    let chain = chain(vec![
        listing(
            Protocol::Blockscout,
            blockscout(Arc::clone(&failed), true).await,
        ),
        listing(Protocol::Routescan, routescan(Arc::clone(&served)).await),
    ])
    .await;
    let found = chain.nfts(&owner()).await.unwrap();
    let ids: Vec<String> = found
        .items
        .iter()
        .map(|item| item.asset.to_string())
        .collect();
    assert_eq!(ids, [ape(1), item()]);
    assert!(!failed.lock().unwrap().is_empty());
    let served = served.lock().unwrap();
    assert!(
        served.iter().any(|path| path.contains("next=more")),
        "{served:?}"
    );
    assert!(served.iter().any(|path| path.contains("erc1155-holdings")));
}

fn sent(asset: &str, units: &str) -> Transfer {
    Transfer {
        account: 0,
        from: owner(),
        to: Address::parse(ChainFamily::Evm, FRIEND).unwrap(),
        asset: asset.parse().unwrap(),
        amount: Amount::parse(units, 0).unwrap(),
        max: false,
        memo: None,
    }
}

#[tokio::test]
async fn nft_sends_are_the_standard_safe_transfers_and_read_back_as_the_nft() {
    let chain = chain(Vec::new()).await;
    for (asset, units, selector) in [(ape(1), "1", "42842e0e"), (item(), "2", "f242432a")] {
        let (plan, fee) = chain.prepare(&sent(&asset, units)).await.unwrap();
        assert!(!fee.expected().is_zero());
        let signed = chain.sign(&plan, &seed()).unwrap();
        let decoded = chain.decode(&signed).unwrap();
        check_signed(chain.planned(&plan), &decoded).unwrap();
        assert_eq!(decoded.asset, asset.parse::<AssetId>().unwrap());
        assert_eq!(decoded.amount.to_string(), units);
        let raw = bahaa_types::hex::encode(&signed.bytes);
        assert!(raw.contains(selector), "{selector}");
        let (faster, _) = chain.replace(&signed, 0, Replace::SpeedUp).await.unwrap();
        assert_eq!(chain.planned(&faster).asset, decoded.asset);
        let again = chain
            .decode(&chain.sign(&faster, &seed()).unwrap())
            .unwrap();
        check_signed(chain.planned(&faster), &again).unwrap();
    }
    let mut all = sent(&item(), "1");
    all.max = true;
    let (plan, _) = chain.prepare(&all).await.unwrap();
    assert_eq!(chain.planned(&plan).amount.to_string(), "3");
}

#[tokio::test]
async fn only_held_nfts_are_sent_without_memos_and_within_what_is_held() {
    let chain = chain(Vec::new()).await;
    let mut memo = sent(&ape(1), "1");
    memo.memo = Some("hi".to_owned());
    let mut back = sent(&ape(1), "1");
    back.to = owner();
    for (transfer, refused) in [
        (
            sent(&ape(2), "1"),
            ChainError::Invalid("this wallet doesn't hold that NFT"),
        ),
        (
            sent(&ape(1), "2"),
            ChainError::Invalid("an ERC-721 NFT is one of a kind, send just 1"),
        ),
        (
            sent(&item(), "4"),
            ChainError::Invalid("this wallet holds fewer of that NFT"),
        ),
        (
            sent(&format!("ethereum/nft/{PLAIN}/5"), "1"),
            ChainError::Invalid("this is not an NFT contract"),
        ),
        (memo, ChainError::Unsupported("memos on NFT transfers")),
        (back, ChainError::Invalid("send the NFT to another account")),
    ] {
        assert_eq!(chain.prepare(&transfer).await.err(), Some(refused));
    }
}
