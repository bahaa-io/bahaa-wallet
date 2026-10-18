#![allow(clippy::unwrap_used)]

mod common;

use std::sync::{Arc, Mutex};

use bahaa_chain::ChainError;
use bahaa_config::{Endpoint, Network, Protocol, Registry};
use bahaa_crypto::SecretString;
use bahaa_engine::{Engine, EngineError, SendRequest};
use bahaa_types::{Amount, ChainFamily, NetworkId};
use common::{HARDHAT, HARDHAT_0, Scratch, config, until};
use serde_json::{Value, json};
use url::Url;

const MAINNET: &str = "17a3a92992aabea785a7a090985a265cd31f323d849da51239737e321fb05569";
const TESTNET: &str = "823f81f306ff02694f935cf5021548e3ce2b86b529812af6a12148879e95a128";

type Asked = Arc<Mutex<Vec<String>>>;

async fn tonapi(zero_state: &'static str, asked: &Asked) -> Url {
    let asked = Arc::clone(asked);
    common::rest_node(move |path, _| -> Value {
        asked.lock().unwrap().push(path.to_owned());
        if path == "/liteserver/get_masterchain_info" {
            json!({"init": {"root_hash": zero_state}})
        } else {
            json!({"balance": 2_500_000_000_u64, "status": "active"})
        }
    })
    .await
}

fn ton(id: &str, url: Url) -> Network {
    let mut network = Registry::builtin()
        .unwrap()
        .get(&id.parse().unwrap())
        .unwrap()
        .clone();
    network.endpoints = vec![Endpoint {
        protocol: Protocol::TonApi,
        url,
        features: Vec::new(),
        min_gap_ms: 0,
    }];
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
        ton("ton", tonapi(MAINNET, &main_asked).await),
        ton("ton-testnet", tonapi(TESTNET, &test_asked).await),
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
        (main, "2.5".to_owned())
    );
    assert_eq!(gram.asset.network().as_str(), "ton");
    assert!(
        main_asked
            .lock()
            .unwrap()
            .contains(&format!("/accounts/{main}"))
    );
    assert!(
        !main_asked
            .lock()
            .unwrap()
            .contains(&format!("/accounts/{test}"))
    );
    assert_eq!(*test_asked.lock().unwrap(), Vec::<String>::new());
    let request = SendRequest {
        wallet,
        asset: "ton".parse().unwrap(),
        to: test,
        amount: Amount::parse("1", 9).unwrap(),
    };
    assert_eq!(
        engine.prepare_send(request).await,
        Err(EngineError::Chain(ChainError::Unsupported("sending yet")))
    );
}
