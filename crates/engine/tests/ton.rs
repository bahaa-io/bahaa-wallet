#![allow(clippy::unwrap_used)]

mod common;

use std::time::Duration;

use bahaa_chain::ChainError;
use bahaa_config::{ChainKind, Network, Registry};
use bahaa_crypto::SecretString;
use bahaa_engine::{Arrival, Engine, EngineError, Event, Received, SendRequest};
use bahaa_types::{Amount, ChainFamily, NetworkId};
use common::{
    Asked, GET_ACCOUNT_STATE, GET_BLOCK_PROOF, HARDHAT, HARDHAT_0, Scratch, config, until,
};
use url::Url;

const MAINNET: &str = "17a3a92992aabea785a7a090985a265cd31f323d849da51239737e321fb05569";
const TESTNET: &str = "823f81f306ff02694f935cf5021548e3ce2b86b529812af6a12148879e95a128";
const TON_0: &str = "0:c73162b34cf19667210af3180d6fcfb88df1678cbbbe731f38642534cc6951e4";
const RECORDING: &str = include_str!("fixtures/ton.json");
const TON_PHRASE: &str = "dose ice enrich trigger test dove century still betray gas diet dune use other base gym mad law immense village world example praise game";

fn ton(id: &str, config: Url) -> Network {
    let mut network = common::ton_network(id, config);
    network.tokens.clear();
    network
}

#[tokio::test]
async fn ton_accounts_follow_each_networks_wallet_id_and_show_gram() {
    let scratch = Scratch::new();
    let (main_asked, test_asked) = (Asked::default(), Asked::default());
    let dead = Url::parse("http://127.0.0.1:9/").unwrap();
    let registry = Registry::with_networks(vec![
        common::network("bsc", dead.clone()),
        common::network("bsc-testnet", dead),
        ton(
            "ton",
            common::liteserver(RECORDING, MAINNET, &main_asked).await,
        ),
        ton(
            "ton-testnet",
            common::liteserver(RECORDING, TESTNET, &test_asked).await,
        ),
    ])
    .unwrap();
    let engine = Engine::start(config(&scratch, registry, None)).unwrap();
    let mut portfolio = engine.portfolio();
    engine.create(SecretString::from("pw")).await.unwrap();
    let wallet = engine
        .import(SecretString::from(HARDHAT), SecretString::from(""))
        .await
        .unwrap();
    let accounts = engine.accounts().await.unwrap();
    let shape: Vec<(ChainFamily, Option<String>)> = accounts
        .iter()
        .map(|account| {
            (
                account.family,
                account.network.as_ref().map(NetworkId::to_string),
            )
        })
        .collect();
    assert_eq!(
        shape,
        [
            (ChainFamily::Evm, None),
            (ChainFamily::Ton, Some("ton".to_owned())),
            (ChainFamily::Ton, Some("ton-testnet".to_owned())),
        ]
    );
    assert_eq!(accounts[0].address.to_string(), HARDHAT_0);
    let (main, test) = (accounts[1].address, accounts[2].address);
    assert_ne!(main, test);
    let filled = until(&mut portfolio, |p| {
        p.holdings.iter().any(|holding| holding.symbol == "GRAM")
    })
    .await;
    let gram = filled
        .holdings
        .iter()
        .find(|holding| holding.symbol == "GRAM")
        .unwrap();
    assert_eq!(
        (gram.address, gram.amount.to_string()),
        (main, "0.000063599".to_owned())
    );
    assert_eq!(gram.asset.network().as_str(), "ton");
    let hash = |address: &bahaa_types::Address| match address {
        bahaa_types::Address::Ton { hash, .. } => common::hex(hash),
        _ => panic!("not a TON address"),
    };
    let asked = main_asked.lock().unwrap().clone();
    assert!(asked.contains(&(GET_BLOCK_PROOF, "99746941".to_owned())));
    assert!(asked.contains(&(GET_ACCOUNT_STATE, hash(&main))));
    assert!(!asked.contains(&(GET_ACCOUNT_STATE, hash(&test))));
    assert_eq!(*test_asked.lock().unwrap(), Vec::new());
    let request = SendRequest {
        wallet,
        asset: "ton".parse().unwrap(),
        to: test,
        amount: Amount::parse("1", 9).unwrap(),
    };
    assert!(matches!(
        engine.prepare_send(request).await,
        Err(EngineError::Chain(ChainError::Insufficient { symbol, available, .. }))
            if symbol == "GRAM" && available.to_string() == "0.000063599"
    ));
    assert!(
        main_asked
            .lock()
            .unwrap()
            .contains(&(GET_ACCOUNT_STATE, hash(&test)))
    );
}

fn received(id: &str, from: &str, lt: u64, time: u64) -> Arrival {
    let received = Received {
        id: id.to_owned(),
        index: 0,
        network: "ton".parse().unwrap(),
        from: bahaa_types::Address::parse(ChainFamily::Ton, from).unwrap(),
        to: bahaa_types::Address::parse(ChainFamily::Ton, TON_0).unwrap(),
        asset: "ton".parse().unwrap(),
        amount: Amount::parse("0.0001", 9).unwrap(),
        block: lt,
        time,
    };
    Arrival {
        received,
        cautions: Vec::new(),
    }
}

#[tokio::test]
async fn incoming_gram_joins_the_wallets_history_from_proven_state() {
    let scratch = Scratch::new();
    let asked = Asked::default();
    let registry = Registry::with_networks(vec![ton(
        "ton",
        common::liteserver(RECORDING, MAINNET, &asked).await,
    )])
    .unwrap();
    let engine = Engine::start(config(&scratch, registry, None)).unwrap();
    let mut events = engine.events();
    engine.create(SecretString::from("pw")).await.unwrap();
    engine
        .import(SecretString::from(HARDHAT), SecretString::from(""))
        .await
        .unwrap();
    let fresh: Vec<Arrival> = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Ok(Event::Received(entries)) = events.recv().await {
                return entries;
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(
        fresh,
        [
            received(
                "90098ab1a048a271603eef7b2b4bea387ed0303be5afe7fe0a0aa5a9f5402d34",
                "0:dd8f0a38d31cd40b9a7b9582f820ecd096ba7e884bd35c3c8782f8a3379bc4f8",
                88_210_222_000_007,
                1_783_275_994
            ),
            received(
                "e0e956c2bf8370c17da7fa93b366f526fe3a06c7bb999448fab1dbd16cff7d55",
                "0:a4a6b4d9847bf95cd37230ab1053825c734e6508d0fa8c23e33b18325c5f34f0",
                88_208_524_000_001,
                1_783_275_313
            ),
        ]
    );
    let incoming = engine.incoming().await.unwrap();
    assert_eq!(incoming.transfers, fresh);
    let coverage = &incoming.coverage[0];
    assert_eq!(
        (
            coverage.network.as_str(),
            coverage.address.to_string(),
            coverage.next,
            coverage.complete_from,
            coverage.complete_since
        ),
        (
            "ton",
            TON_0.to_owned(),
            1_783_275_694,
            1_783_274_391,
            1_783_274_391
        )
    );
}

#[tokio::test]
async fn ton_token_sends_are_quoted_from_proven_contract_state() {
    let scratch = Scratch::new();
    let asked = Asked::default();
    let network = common::ton_network("ton", common::liteserver(RECORDING, MAINNET, &asked).await);
    let usdt = network.tokens[0].contract;
    let registry = Registry::with_networks(vec![network]).unwrap();
    let engine = Engine::start(config(&scratch, registry, None)).unwrap();
    engine.create(SecretString::from("pw")).await.unwrap();
    let wallet = engine
        .import(SecretString::from(HARDHAT), SecretString::from(""))
        .await
        .unwrap();
    let request = SendRequest {
        wallet,
        asset: bahaa_types::AssetId::Token("ton".parse().unwrap(), usdt),
        to: bahaa_types::Address::parse(ChainFamily::Ton, TON_0).unwrap(),
        amount: Amount::parse("1", 6).unwrap(),
    };
    assert_eq!(
        engine.prepare_send(request).await,
        Err(EngineError::Chain(ChainError::Insufficient {
            symbol: "USDT".to_owned(),
            needed: Amount::parse("1", 6).unwrap(),
            available: Amount::parse("0", 6).unwrap(),
        }))
    );
}

#[tokio::test]
async fn the_newest_trusted_ton_block_is_kept_for_the_next_start() {
    let scratch = Scratch::new();
    let asked = Asked::default();
    let mut network = ton("ton", common::liteserver(RECORDING, MAINNET, &asked).await);
    if let ChainKind::Ton { checkpoint, .. } = &mut network.kind {
        *checkpoint = common::PREVIOUS_KEY;
    }
    let registry = Registry::with_networks(vec![network]).unwrap();
    let engine = Engine::start(config(&scratch, registry.clone(), None)).unwrap();
    let mut portfolio = engine.portfolio();
    engine.create(SecretString::from("pw")).await.unwrap();
    engine
        .import(SecretString::from(HARDHAT), SecretString::from(""))
        .await
        .unwrap();
    until(&mut portfolio, |p| {
        p.holdings.iter().any(|holding| holding.symbol == "GRAM")
    })
    .await;
    engine.shutdown().await.unwrap();
    let proofs = |asked: &Asked| -> Vec<String> {
        asked
            .lock()
            .unwrap()
            .iter()
            .filter(|(function, _)| *function == GET_BLOCK_PROOF)
            .map(|(_, known)| known.clone())
            .collect()
    };
    assert_eq!(proofs(&asked), ["99727067"]);
    asked.lock().unwrap().clear();
    let engine = Engine::start(config(&scratch, registry, None)).unwrap();
    let mut portfolio = engine.portfolio();
    engine.unlock(SecretString::from("pw")).await.unwrap();
    until(&mut portfolio, |p| {
        p.holdings.iter().any(|holding| holding.symbol == "GRAM")
    })
    .await;
    assert_eq!(proofs(&asked), ["99746941"]);
}

#[tokio::test]
async fn a_ton_phrase_opens_only_its_ton_accounts() {
    let scratch = Scratch::new();
    let dead = Url::parse("http://127.0.0.1:9/").unwrap();
    let asked = Asked::default();
    let registry = Registry::with_networks(vec![
        common::network("bsc", dead.clone()),
        ton("ton", common::liteserver(RECORDING, MAINNET, &asked).await),
        ton("ton-testnet", dead),
    ])
    .unwrap();
    let engine = Engine::start(config(&scratch, registry, None)).unwrap();
    engine.create(SecretString::from("pw")).await.unwrap();
    let wallet = engine
        .import(SecretString::from(TON_PHRASE), SecretString::from(""))
        .await
        .unwrap();
    let wallets = engine.wallets().await.unwrap();
    assert_eq!(
        wallets[0].kind,
        bahaa_engine::WalletKind::Ton(bahaa_types::TonContract::W5)
    );
    let asked = asked.lock().unwrap().clone();
    for hash in [
        "dbb94cc66ee709eb191e2e3af93b9b3b036bbcc69fb70e5c426cef0b9b7930a1",
        "f0cc9cd9b07dab208f52bf892b006221227d76a448428f82d870a6bc8dd5d377",
    ] {
        assert!(
            asked.contains(&(GET_ACCOUNT_STATE, hash.to_owned())),
            "{hash}"
        );
    }
    let accounts = engine.accounts().await.unwrap();
    let shape: Vec<(ChainFamily, Option<String>, String)> = accounts
        .iter()
        .map(|account| {
            (
                account.family,
                account.network.as_ref().map(NetworkId::to_string),
                account.address.to_string(),
            )
        })
        .collect();
    assert_eq!(
        shape,
        [
            (
                ChainFamily::Ton,
                Some("ton".to_owned()),
                "0:dbb94cc66ee709eb191e2e3af93b9b3b036bbcc69fb70e5c426cef0b9b7930a1".to_owned()
            ),
            (
                ChainFamily::Ton,
                Some("ton-testnet".to_owned()),
                "0:14bae359b81d05e6d760007a109b21446c29697ee169ffc884bde0d5a0380d3e".to_owned()
            ),
        ]
    );
    let bnb = SendRequest {
        wallet,
        asset: "bsc".parse().unwrap(),
        to: bahaa_types::Address::parse(ChainFamily::Evm, HARDHAT_0).unwrap(),
        amount: Amount::parse("1", 18).unwrap(),
    };
    assert_eq!(
        engine.prepare_send(bnb).await,
        Err(EngineError::Chain(ChainError::Key(
            bahaa_crypto::DeriveError::TonOnly
        )))
    );
}

#[tokio::test]
async fn a_ton_phrase_waits_until_its_wallet_contract_can_be_checked() {
    let scratch = Scratch::new();
    let dead = Url::parse("http://127.0.0.1:9/").unwrap();
    let registry = Registry::with_networks(vec![ton("ton", dead)]).unwrap();
    let engine = Engine::start(config(&scratch, registry, None)).unwrap();
    engine.create(SecretString::from("pw")).await.unwrap();
    assert!(matches!(
        engine
            .import(SecretString::from(TON_PHRASE), SecretString::from(""))
            .await,
        Err(EngineError::Chain(ChainError::Network(_)))
    ));
    assert_eq!(engine.wallets().await.unwrap(), Vec::new());
    assert_eq!(engine.accounts().await.unwrap(), Vec::new());
    engine
        .import(SecretString::from(HARDHAT), SecretString::from(""))
        .await
        .unwrap();
}

#[tokio::test]
async fn a_v4r2_ton_wallet_shows_its_v4r2_address() {
    let scratch = Scratch::new();
    {
        let mut keyring = bahaa_engine::Keyring::new(
            scratch.0.join("data").join("vault.bin"),
            bahaa_vault::KdfParams {
                memory_kib: 64,
                iterations: 1,
                lanes: 1,
            },
        );
        keyring.create(b"pw").unwrap();
        let id = keyring.import(TON_PHRASE, "").unwrap();
        keyring
            .set_ton_contract(id, bahaa_types::TonContract::V4R2)
            .unwrap();
    }
    let dead = Url::parse("http://127.0.0.1:9/").unwrap();
    let registry = Registry::with_networks(vec![ton("ton", dead)]).unwrap();
    let engine = Engine::start(config(&scratch, registry, None)).unwrap();
    engine.unlock(SecretString::from("pw")).await.unwrap();
    let accounts = engine.accounts().await.unwrap();
    assert_eq!(accounts.len(), 1);
    assert_eq!(
        accounts[0].address.to_string(),
        "0:f0cc9cd9b07dab208f52bf892b006221227d76a448428f82d870a6bc8dd5d377"
    );
}
