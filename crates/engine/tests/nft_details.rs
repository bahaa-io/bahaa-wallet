#![allow(clippy::unwrap_used)]

mod common;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use bahaa_config::{Endpoint, Feature, KnownCollection, Protocol, Registry};
use bahaa_crypto::SecretString;
use bahaa_engine::{Collectible, Collectibles, Details, Engine, Fake, Gateways, Mark, Trust};
use bahaa_testkit::Reply;
use bahaa_types::{Address, ChainFamily};
use common::{Scratch, config, failure, result, until};
use serde_json::{Value, json};
use tokio::sync::watch;
use url::Url;

const OWNER: &str = "0x9965507d1a55bcc2695c58ba16fb37d819b0a4dc";
const KNOWN: &str = "0x1111111111111111111111111111111111111111";
const COPY: &str = "0x2222222222222222222222222222222222222222";
const PLAIN: &str = "0x3333333333333333333333333333333333333333";
const CID: &str = "QmeSjSinHpPnmXmspMjwiXyN6zS4E9zccariGR3jxcaWtq";
const PNG: &str = "89504e470d0a1a0a0000000d494844520000000200000002080600000072b60d240000001149444154789c63382167f31f8419600c004b30088548ced6ad0000000049454e44ae426082";

type Asked = Arc<Mutex<Vec<String>>>;

fn word(value: u64) -> String {
    format!("{value:064x}")
}

fn text(value: &str) -> String {
    let mut body = bahaa_types::hex::encode(value.as_bytes());
    while !body.len().is_multiple_of(64) {
        body.push('0');
    }
    format!("0x{}{}{body}", word(32), word(value.len() as u64))
}

fn contract(to: &str, data: &str) -> Value {
    let (name, id) = match to {
        KNOWN => ("BAT", 1),
        COPY => ("Bored Apes Test Club", 2),
        PLAIN => ("Plain Cats", 3),
        _ => return failure(3, "execution reverted"),
    };
    let supports_721 = format!("0x01ffc9a780ac58cd{}", "0".repeat(56));
    match data {
        "0x06fdde03" => result(json!(text(name))),
        d if d == supports_721 => result(json!(format!("0x{}", word(1)))),
        d if d == format!("0x6352211e{}", word(id)) => {
            result(json!(format!("0x{:0>64}", &OWNER[2..])))
        }
        d if d == format!("0xc87b56dd{}", word(id)) => {
            result(json!(text(&format!("ipfs://{CID}/{id}"))))
        }
        _ => failure(3, "execution reverted"),
    }
}

fn answer(call: &Value) -> Value {
    let params = &call["params"];
    match call["method"].as_str().unwrap() {
        "eth_chainId" => result(json!("0x1")),
        "eth_call" => contract(
            &params[0]["to"].as_str().unwrap().to_ascii_lowercase(),
            params[0]["data"].as_str().unwrap(),
        ),
        "eth_blockNumber" => result(json!("0x100")),
        "eth_getBalance" | "eth_getTransactionCount" => result(json!("0x0")),
        "eth_getLogs" => result(json!([])),
        _ => failure(-32601, "no such method"),
    }
}

fn listed(contract: &str, id: u64) -> Value {
    json!({"id": id.to_string(), "token_type": "ERC-721", "value": "1", "token": {"address_hash": contract, "name": "Listed", "reputation": "ok"}})
}

async fn gateway(asked: Asked, broken: bool) -> Url {
    let url = bahaa_testkit::serve(move |request| {
        let path = request.path().to_owned();
        asked.lock().unwrap().push(path.clone());
        if broken {
            return Reply::json(503, &json!({"error": "busy"}));
        }
        if path.rsplit('/').next().is_some_and(|name| name.contains('.')) {
            return Reply::bytes(200, "image/png", bahaa_types::hex::decode(PNG).unwrap());
        }
        let id = path.rsplit('/').next().unwrap().to_owned();
        Reply::json(
            200,
            &json!({"name": format!("Item #{id}"), "description": format!("About {id}"), "image": format!("ipfs://{CID}/{id}.png"), "attributes": []}),
        )
    })
    .await;
    url.join("ipfs/").unwrap()
}

async fn registry() -> Registry {
    let rpc = common::node(|call| (answer(call), Duration::ZERO)).await;
    let listing = bahaa_testkit::serve(|request| {
        if request.path().contains("/nft") {
            Reply::json(
                200,
                &json!({"items": [listed(KNOWN, 1), listed(COPY, 2), listed(PLAIN, 3)], "next_page_params": null}),
            )
        } else {
            Reply::json(404, &json!({"message": "not here"}))
        }
    })
    .await;
    let mut network = common::network("ethereum", rpc);
    network.tokens.clear();
    network.collections = vec![KnownCollection {
        address: Address::parse(ChainFamily::Evm, KNOWN).unwrap(),
        name: "Bored Apes Test".to_owned(),
    }];
    network.endpoints.push(Endpoint {
        protocol: Protocol::Blockscout,
        url: listing,
        features: vec![Feature::Nfts],
        min_gap_ms: 0,
        history_gap_ms: 0,
    });
    Registry::with_networks(vec![network]).unwrap()
}

fn item<'a>(listed: &'a Collectibles, contract: &str) -> &'a Collectible {
    listed
        .items
        .iter()
        .find(|item| {
            item.nft
                .asset
                .to_string()
                .to_ascii_lowercase()
                .contains(contract)
        })
        .unwrap()
}

fn described(listed: &Collectibles, contract: &str) -> bool {
    listed.items.len() == 3 && item(listed, contract).details.is_some()
}

async fn quiet(nfts: &mut watch::Receiver<Collectibles>) {
    until(nfts, |listed| {
        listed.items.len() == 3 && listed.health.iter().all(|health| !health.loading)
    })
    .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
}

async fn started(scratch: &Scratch) -> (Engine, watch::Receiver<Collectibles>, Asked, Asked) {
    let (down, up): (Asked, Asked) = (Asked::default(), Asked::default());
    let engine = Engine::start(bahaa_engine::EngineConfig {
        gateways: Some(Gateways {
            ipfs: vec![
                gateway(Arc::clone(&down), true).await,
                gateway(Arc::clone(&up), false).await,
            ],
            arweave: Vec::new(),
        }),
        ..config(scratch, registry().await, None)
    })
    .unwrap();
    let nfts = engine.nfts();
    engine.create(SecretString::from("pw")).await.unwrap();
    engine
        .watch(Address::parse(ChainFamily::Evm, OWNER).unwrap(), true)
        .await
        .unwrap();
    (engine, nfts, down, up)
}

#[tokio::test]
async fn only_shown_nfts_get_their_details_and_known_collections_are_trusted() {
    let scratch = Scratch::new();
    let (engine, mut nfts, down, up) = started(&scratch).await;
    let first = until(&mut nfts, |listed| described(listed, KNOWN)).await;

    let known = item(&first, KNOWN);
    assert_eq!(
        (known.nft.trust, known.shown, &known.fake),
        (Trust::Trusted, true, &None)
    );
    assert_eq!(
        known.nft.collection.as_ref().unwrap().name.as_deref(),
        Some("Bored Apes Test")
    );
    assert_eq!(
        known.details,
        Some(Details {
            name: Some("Item #1".to_owned()),
            about: Some("About 1".to_owned()),
            image: Some(format!("ipfs://{CID}/1.png")),
        })
    );
    let copy = item(&first, COPY);
    assert_eq!(
        (copy.nft.trust, copy.shown, &copy.fake),
        (
            Trust::Flagged,
            false,
            &Some(Fake::Copies("Bored Apes Test".to_owned()))
        )
    );
    let plain = item(&first, PLAIN);
    assert_eq!(
        (plain.nft.trust, plain.shown, &plain.fake),
        (Trust::Unknown, false, &None)
    );
    quiet(&mut nfts).await;
    assert_eq!(*up.lock().unwrap(), [format!("/ipfs/{CID}/1")]);
    assert_eq!(*down.lock().unwrap(), [format!("/ipfs/{CID}/1")]);

    let plain = plain.nft.asset.clone();
    engine.mark_nft(plain, Some(Mark::Shown)).await.unwrap();
    let marked = until(&mut nfts, |listed| described(listed, PLAIN)).await;
    assert_eq!(
        item(&marked, PLAIN)
            .details
            .as_ref()
            .unwrap()
            .name
            .as_deref(),
        Some("Item #3")
    );
    assert!(item(&marked, COPY).details.is_none());

    engine.lock().await.unwrap();
    until(&mut nfts, |listed| listed.items.is_empty()).await;
    engine.unlock(SecretString::from("pw")).await.unwrap();
    until(&mut nfts, |listed| {
        described(listed, KNOWN) && described(listed, PLAIN)
    })
    .await;
    quiet(&mut nfts).await;
    assert_eq!(
        *up.lock().unwrap(),
        [format!("/ipfs/{CID}/1"), format!("/ipfs/{CID}/3")]
    );
}

#[tokio::test]
async fn pictures_are_read_only_when_asked_for_and_only_for_shown_nfts() {
    let scratch = Scratch::new();
    let (engine, mut nfts, _down, up) = started(&scratch).await;
    let first = until(&mut nfts, |listed| described(listed, KNOWN)).await;
    let (known, plain) = (
        item(&first, KNOWN).nft.asset.clone(),
        item(&first, PLAIN).nft.asset.clone(),
    );
    quiet(&mut nfts).await;
    assert!(
        nfts.borrow()
            .items
            .iter()
            .all(|item| item.picture.is_none())
    );
    engine.want_picture(plain.clone());
    engine.want_picture(known.clone());
    let painted = until(&mut nfts, |listed| item(listed, KNOWN).picture.is_some()).await;
    let picture = item(&painted, KNOWN).picture.clone().unwrap();
    assert_eq!(
        (picture.width(), picture.height(), picture.pixel(1, 1)),
        (2, 2, [200, 30, 60, 255])
    );
    quiet(&mut nfts).await;
    assert!(item(&nfts.borrow(), PLAIN).picture.is_none());

    engine
        .mark_nft(plain.clone(), Some(Mark::Shown))
        .await
        .unwrap();
    until(&mut nfts, |listed| described(listed, PLAIN)).await;
    engine.want_picture(plain.clone());
    until(&mut nfts, |listed| item(listed, PLAIN).picture.is_some()).await;

    engine.lock().await.unwrap();
    until(&mut nfts, |listed| listed.items.is_empty()).await;
    engine.unlock(SecretString::from("pw")).await.unwrap();
    until(&mut nfts, |listed| {
        item_painted(listed, KNOWN) && item_painted(listed, PLAIN)
    })
    .await;
    engine.want_picture(known);
    engine.want_picture(plain);
    quiet(&mut nfts).await;
    assert_eq!(
        *up.lock().unwrap(),
        [
            format!("/ipfs/{CID}/1"),
            format!("/ipfs/{CID}/1.png"),
            format!("/ipfs/{CID}/3"),
            format!("/ipfs/{CID}/3.png")
        ]
    );
}

fn item_painted(listed: &Collectibles, contract: &str) -> bool {
    listed.items.len() == 3 && item(listed, contract).picture.is_some()
}
