#![allow(clippy::unwrap_used)]

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use bahaa_config::{ChainKind, Checkpoint, Endpoint, Feature, Protocol, Registry};
use bahaa_crypto::SecretString;
use bahaa_engine::{
    Amount, Collectibles, Engine, EngineError, Mark, NftHealth, SendRequest, Spend, Trust, WalletId,
};
use bahaa_testkit::Reply;
use bahaa_types::{Address, AssetId, ChainFamily};
use common::{Asked, HARDHAT, Scratch, config, unhex, until};
use serde_json::{Value, json};
use tokio::sync::watch;

const NFTS: &str = include_str!("../../ton/tests/fixtures/nfts.json");
const MAINNET: &str = "17a3a92992aabea785a7a090985a265cd31f323d849da51239737e321fb05569";
const COLLECTOR: &str = "0:e9d947cff5ce632b85f90f68652b38f12181097d3a9cf1c1c6c1cae6e6b816e3";
const ELSEWHERE: &str =
    "ton/nft/0:d59a7de5e0bd393f56314fb14737675fbf2c7d6b7dd18a6c09bda52ed7aa84d7";

async fn registry(broken: Arc<AtomicBool>) -> Registry {
    let fixture: Value = serde_json::from_str(NFTS).unwrap();
    let lite = common::liteserver(NFTS, MAINNET, &Asked::default()).await;
    let mut network = common::ton_network("ton", lite);
    network.tokens.clear();
    if let ChainKind::Ton { checkpoint, .. } = &mut network.kind {
        let key = &fixture["key"];
        *checkpoint = Checkpoint {
            seqno: u32::try_from(key["seqno"].as_u64().unwrap()).unwrap(),
            root_hash: unhex(key["root_hash"].as_str().unwrap())
                .try_into()
                .unwrap(),
            file_hash: unhex(key["file_hash"].as_str().unwrap())
                .try_into()
                .unwrap(),
        };
    }
    let listing = fixture["tonapi"][COLLECTOR].clone();
    let api = bahaa_testkit::serve(move |request| {
        if request.path().ends_with("/liteserver/get_masterchain_info") {
            return Reply::json(200, &json!({"init": {"root_hash": MAINNET}}));
        }
        if !request.path().contains("/nfts") {
            return Reply::json(404, &json!({"error": "method"}));
        }
        if broken.load(Ordering::SeqCst) {
            Reply::json(503, &json!({"error": "busy"}))
        } else if request.path().contains(COLLECTOR) {
            Reply::json(200, &listing)
        } else {
            Reply::json(200, &json!({"nft_items": []}))
        }
    })
    .await;
    network.endpoints.push(Endpoint {
        protocol: Protocol::TonApi,
        url: api,
        features: vec![Feature::Nfts],
        min_gap_ms: 0,
        history_gap_ms: 0,
    });
    Registry::with_networks(vec![network]).unwrap()
}

fn vault(scratch: &Scratch) -> bahaa_engine::Keyring {
    let mut keyring = bahaa_engine::Keyring::new(
        scratch.0.join("data").join("vault.bin"),
        bahaa_vault::KdfParams {
            memory_kib: 64,
            iterations: 1,
            lanes: 1,
        },
    );
    keyring.create(b"pw").unwrap();
    keyring
}

fn shown(listed: &Collectibles) -> usize {
    listed.items.iter().filter(|item| item.shown).count()
}

fn marked(listed: &Collectibles, asset: &AssetId) -> Option<(bool, Option<Mark>)> {
    listed
        .items
        .iter()
        .find(|item| item.nft.asset == *asset)
        .map(|item| (item.shown, item.mark))
}

fn with(listed: &Collectibles, trust: Trust) -> AssetId {
    listed
        .items
        .iter()
        .find(|item| item.nft.trust == trust)
        .unwrap()
        .nft
        .asset
        .clone()
}

struct Listed {
    engine: Engine,
    nfts: watch::Receiver<Collectibles>,
    broken: Arc<AtomicBool>,
    wallet: WalletId,
    first: Collectibles,
}

async fn listed(scratch: &Scratch) -> Listed {
    vault(scratch);
    let broken = Arc::new(AtomicBool::new(false));
    let engine = Engine::start(config(scratch, registry(Arc::clone(&broken)).await, None)).unwrap();
    let mut nfts = engine.nfts();
    engine.unlock(SecretString::from("pw")).await.unwrap();
    let collector = Address::parse(ChainFamily::Ton, COLLECTOR).unwrap();
    let wallet = engine.watch(collector, true).await.unwrap();
    let first = until(&mut nfts, |listed| listed.items.len() == 19).await;
    Listed {
        engine,
        nfts,
        broken,
        wallet,
        first,
    }
}

#[tokio::test]
async fn proven_nfts_show_only_when_trusted_unless_the_user_decides() {
    let scratch = Scratch::new();
    let Listed {
        engine,
        mut nfts,
        wallet,
        first,
        ..
    } = listed(&scratch).await;
    let collector = Address::parse(ChainFamily::Ton, COLLECTOR).unwrap();
    assert_eq!(shown(&first), 17);
    assert!(
        first
            .items
            .iter()
            .all(|item| item.wallet == wallet && item.address == collector)
    );
    assert!(
        first
            .items
            .iter()
            .all(|item| item.shown == (item.nft.trust == Trust::Trusted))
    );
    let [health] = first.health.as_slice() else {
        panic!("one network expected: {:?}", first.health);
    };
    assert!(matches!(
        health,
        NftHealth {
            loading: false,
            problem: None,
            capped: false,
            checked_at: Some(_),
            ..
        }
    ));
    let (unknown, trusted) = (with(&first, Trust::Unknown), with(&first, Trust::Trusted));
    engine
        .mark_nft(unknown.clone(), Some(Mark::Shown))
        .await
        .unwrap();
    engine
        .mark_nft(trusted.clone(), Some(Mark::Hidden))
        .await
        .unwrap();
    let changed = until(&mut nfts, |listed| {
        marked(listed, &unknown) == Some((true, Some(Mark::Shown)))
            && marked(listed, &trusted) == Some((false, Some(Mark::Hidden)))
    })
    .await;
    assert_eq!(shown(&changed), 17);
    for foreign in [
        AssetId::Native("ton".parse().unwrap()),
        ELSEWHERE.parse().unwrap(),
    ] {
        assert!(matches!(
            engine.mark_nft(foreign, Some(Mark::Shown)).await,
            Err(EngineError::Chain(_))
        ));
    }
    engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn listings_and_choices_survive_a_lock_even_when_the_source_is_down() {
    let scratch = Scratch::new();
    let Listed {
        engine,
        mut nfts,
        broken,
        first,
        ..
    } = listed(&scratch).await;
    let unknown = with(&first, Trust::Unknown);
    engine
        .mark_nft(unknown.clone(), Some(Mark::Shown))
        .await
        .unwrap();
    until(&mut nfts, |listed| shown(listed) == 18).await;
    broken.store(true, Ordering::SeqCst);
    engine.lock().await.unwrap();
    assert_eq!(nfts.borrow().items, Vec::new());
    engine.unlock(SecretString::from("pw")).await.unwrap();
    let again = until(&mut nfts, |listed| {
        listed.items.len() == 19
            && listed
                .health
                .iter()
                .any(|health| !health.loading && health.problem.is_some())
    })
    .await;
    assert_eq!(marked(&again, &unknown), Some((true, Some(Mark::Shown))));
    engine.mark_nft(unknown.clone(), None).await.unwrap();
    let reset = until(&mut nfts, |listed| {
        marked(listed, &unknown) == Some((false, None))
    })
    .await;
    assert_eq!(shown(&reset), 17);
    engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn an_nft_is_never_sent_to_its_own_address() {
    let scratch = Scratch::new();
    let wallet = vault(&scratch).import(HARDHAT, "").unwrap();
    let engine = Engine::start(config(
        &scratch,
        registry(Arc::new(AtomicBool::new(false))).await,
        None,
    ))
    .unwrap();
    engine.unlock(SecretString::from("pw")).await.unwrap();
    let asset: AssetId = ELSEWHERE.parse().unwrap();
    let item = *asset.nft().unwrap().address();
    let sent = engine
        .prepare_send(SendRequest {
            wallet,
            account: 0,
            asset,
            to: item,
            amount: Spend::Exact(Amount::parse("1", 0).unwrap()),
            memo: None,
        })
        .await;
    assert!(
        matches!(&sent, Err(EngineError::Chain(bahaa_chain::ChainError::Invalid(text))) if text.contains("NFT's own address")),
        "{sent:?}"
    );
    engine.shutdown().await.unwrap();
}
