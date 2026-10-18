#![allow(clippy::unwrap_used)]

mod common;

use std::sync::Arc;

use bahaa_chain::{Chain, ChainError};
use bahaa_config::{Endpoint, Network, Protocol, Registry};
use bahaa_ton::TonChain;
use bahaa_types::{Address, AssetId, ChainFamily};
use serde_json::json;
use url::Url;

use common::{Log, node};

const USDT: &str = "0:b113a994b5024a16719f69139328eb759596c38a25f59028b146fecdc3621dfe";
const HOLDER: &str = "0:f8b8635d0786b77f9da660e65fd31d670a675dd4302facba692f46b571eb5360";

fn ton_on(endpoints: &[(Protocol, &Url)]) -> Network {
    let registry = Registry::builtin().unwrap();
    let mut network = registry.get(&"ton".parse().unwrap()).unwrap().clone();
    network.endpoints = endpoints
        .iter()
        .map(|(protocol, url)| Endpoint {
            protocol: *protocol,
            url: (*url).clone(),
            features: Vec::new(),
            min_gap_ms: 0,
            history_gap_ms: 0,
        })
        .collect();
    network
}

fn holder() -> Address {
    Address::parse(ChainFamily::Ton, HOLDER).unwrap()
}

fn assets() -> Vec<AssetId> {
    let master = Address::parse(ChainFamily::Ton, USDT).unwrap();
    vec![
        "ton".parse().unwrap(),
        AssetId::Token("ton".parse().unwrap(), master),
    ]
}

#[tokio::test]
async fn balances_are_never_read_from_http_providers() {
    let (center_log, api_log) = (Log::default(), Log::default());
    let center = node(Arc::clone(&center_log), |_, _| {
        (200, json!({"ok": true, "result": "1561453563"}))
    })
    .await;
    let api = node(Arc::clone(&api_log), |_, _| {
        (
            200,
            json!({"balance": 9_000_000_000_u64, "status": "active"}),
        )
    })
    .await;
    let chain = TonChain::new(ton_on(&[
        (Protocol::TonCenter, &center),
        (Protocol::TonApi, &api),
    ]))
    .unwrap();
    for asset in assets() {
        assert!(matches!(
            chain.balances(&holder(), &[asset]).await,
            Err(ChainError::Network(_))
        ));
    }
    assert_eq!(*center_log.lock().unwrap(), Vec::<String>::new());
    assert_eq!(*api_log.lock().unwrap(), Vec::<String>::new());
}

#[tokio::test]
async fn custom_tokens_and_foreign_addresses_are_refused() {
    let url = node(Log::default(), |_, _| (404, json!("missing"))).await;
    let chain = TonChain::new(ton_on(&[(Protocol::TonApi, &url)])).unwrap();
    let custom = AssetId::Token(
        "ton".parse().unwrap(),
        Address::Ton {
            workchain: 0,
            hash: [9; 32],
        },
    );
    assert_eq!(
        chain.balances(&holder(), &[custom]).await,
        Err(ChainError::Unsupported("custom TON tokens yet"))
    );
    assert!(matches!(
        chain.balances(&Address::Evm([1; 20]), &assets()).await,
        Err(ChainError::Invalid(_))
    ));
    assert!(matches!(
        chain.balances(&holder(), &["tron".parse().unwrap()]).await,
        Err(ChainError::Invalid(_))
    ));
}
