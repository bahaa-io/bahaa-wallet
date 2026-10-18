#![allow(clippy::unwrap_used)]

mod common;

use std::sync::Arc;

use bahaa_chain::{Chain, ChainError};
use bahaa_config::{Endpoint, Network, Protocol, Registry};
use bahaa_ton::TonChain;
use bahaa_types::{Address, AssetId, ChainFamily};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use serde_json::{Value, json};
use url::Url;

use common::{
    Log, node, proxied_account, proxied_config, proxied_head, proxied_library, recorded, unhex,
};

const FIXTURE: &str = include_str!("fixtures/account_proofs.json");
const ZERO_STATE: &str = "17a3a92992aabea785a7a090985a265cd31f323d849da51239737e321fb05569";
const TESTNET_ZERO_STATE: &str = "823f81f306ff02694f935cf5021548e3ce2b86b529812af6a12148879e95a128";
const USDT: &str = "0:b113a994b5024a16719f69139328eb759596c38a25f59028b146fecdc3621dfe";
const HOLDER: &str = "0:f8b8635d0786b77f9da660e65fd31d670a675dd4302facba692f46b571eb5360";
const FRESH: &str = "0:5c9d2f41661337a80e71c4582b9af006d34e81275c9d2f41661337a80e71c458";
const LIBRARY: &str = "8f452d7a4dfd74066b682365177259ed05734435be76b5fd4bd5d8af2b7c3d68";

fn base64_of(hex: &str) -> String {
    STANDARD.encode(unhex(hex))
}

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

fn amounts(balances: &[bahaa_chain::Balance]) -> Vec<String> {
    balances
        .iter()
        .map(|balance| balance.amount.to_string())
        .collect()
}

struct Center {
    zero_state: &'static str,
}

impl Center {
    fn new(zero_state: &'static str) -> Arc<Self> {
        Arc::new(Self { zero_state })
    }

    fn answer(&self, path: &str) -> (u16, Value) {
        let ok = |result: Value| (200, json!({"ok": true, "result": result}));
        if path == "/v2/getMasterchainInfo" {
            return ok(json!({"init": {"root_hash": base64_of(self.zero_state)}}));
        }
        if path.starts_with("/v2/getAddressBalance?") {
            return ok(json!("1561453563"));
        }
        (404, json!({"ok": false, "error": "not found"}))
    }
}

async fn serve(center: &Arc<Center>, log: Log) -> Url {
    let center = Arc::clone(center);
    node(log, move |path, _| center.answer(path)).await
}

#[tokio::test]
async fn ton_center_reads_gram_but_never_runs_token_contracts() {
    let (center, log) = (Center::new(ZERO_STATE), Log::default());
    let url = serve(&center, Arc::clone(&log)).await;
    let chain = TonChain::new(ton_on(&[(Protocol::TonCenter, &url)])).unwrap();
    for _ in 0..2 {
        let balances = chain.balances(&holder(), &assets()[..1]).await.unwrap();
        assert_eq!(amounts(&balances), ["1.561453563"]);
    }
    assert!(chain.balances(&holder(), &assets()).await.is_err());
    let asked = log.lock().unwrap().clone();
    assert_eq!(
        asked
            .iter()
            .filter(|path| *path == "/v2/getMasterchainInfo")
            .count(),
        1
    );
    assert!(asked.contains(&format!(
        "/v2/getAddressBalance?address={}",
        HOLDER.replace(':', "%3A")
    )));
    assert!(!asked.iter().any(|path| path.contains("runGetMethod")));
}

fn tonapi(path: &str, tampered: bool) -> (u16, Value) {
    let found = |answer: Option<Vec<u8>>, convert: fn(&[u8]) -> Value| {
        answer.map_or_else(
            || (404, json!({"error": "entity not found"})),
            |answer| (200, convert(&answer)),
        )
    };
    if path == "/liteserver/get_masterchain_info" {
        let info = recorded(FIXTURE, &["masterchain_info"]).unwrap();
        return (200, proxied_head(&info, ZERO_STATE));
    }
    if path == format!("/accounts/{HOLDER}") {
        return (
            200,
            json!({"balance": 59_570_010_146_444_835_u64, "status": "active"}),
        );
    }
    if let Some(address) = path.strip_prefix("/liteserver/get_account_state/") {
        let (_, hash) = address.split_once(':').unwrap();
        let mut answer = found(recorded(FIXTURE, &["accounts", hash]), proxied_account);
        if tampered && address == USDT {
            let state = answer.1["state"].as_str().unwrap().to_owned();
            let middle = state.len() / 2;
            let flipped = if &state[middle..=middle] == "0" {
                "1"
            } else {
                "0"
            };
            answer.1["state"] = json!(format!(
                "{}{flipped}{}",
                &state[..middle],
                &state[middle + 1..]
            ));
        }
        return answer;
    }
    if path.starts_with("/liteserver/get_config_all/") {
        return found(recorded(FIXTURE, &["config"]), proxied_config);
    }
    if path == format!("/blockchain/libraries/{LIBRARY}") {
        return found(recorded(FIXTURE, &["libraries", LIBRARY]), proxied_library);
    }
    if path.starts_with("/accounts/") {
        return (404, json!({"error": "entity not found"}));
    }
    (404, json!("missing"))
}

#[tokio::test]
async fn tonapi_token_balances_come_from_contracts_run_on_proven_state() {
    let log = Log::default();
    let url = node(Arc::clone(&log), |path, _| tonapi(path, false)).await;
    let chain = TonChain::new(ton_on(&[(Protocol::TonApi, &url)])).unwrap();
    let balances = chain.balances(&holder(), &assets()).await.unwrap();
    assert_eq!(amounts(&balances), ["59570010.146444835", "94631240.538"]);
    assert!(
        !log.lock()
            .unwrap()
            .iter()
            .any(|path| path.contains("/methods/"))
    );
    let nobody = Address::Ton {
        workchain: 0,
        hash: [7; 32],
    };
    let empty = chain.balances(&nobody, &assets()[..1]).await.unwrap();
    assert_eq!(amounts(&empty), ["0"]);
    let fresh = Address::parse(ChainFamily::Ton, FRESH).unwrap();
    assert_eq!(
        amounts(&chain.balances(&fresh, &assets()).await.unwrap()),
        ["0", "0"]
    );
    let url = node(Log::default(), |path, _| tonapi(path, true)).await;
    let chain = TonChain::new(ton_on(&[(Protocol::TonApi, &url)])).unwrap();
    assert!(matches!(
        chain.balances(&holder(), &assets()).await,
        Err(ChainError::Network(_))
    ));
}

#[tokio::test]
async fn broken_liteservers_other_networks_and_busy_providers_are_passed_over() {
    let (lite_log, testnet_log, busy_log) = (Log::default(), Log::default(), Log::default());
    let lite = node(Arc::clone(&lite_log), |_, _| (500, json!("unused"))).await;
    let testnet = serve(&Center::new(TESTNET_ZERO_STATE), Arc::clone(&testnet_log)).await;
    let busy = node(Arc::clone(&busy_log), |_, _| {
        (429, json!({"error": "Ratelimit exceeded"}))
    })
    .await;
    let api = node(Log::default(), |path, _| tonapi(path, true)).await;
    let chain = TonChain::new(ton_on(&[
        (Protocol::TonLiteservers, &lite),
        (Protocol::TonCenter, &testnet),
        (Protocol::TonCenter, &busy),
        (Protocol::TonApi, &api),
    ]))
    .unwrap();
    let balances = chain.balances(&holder(), &assets()[..1]).await.unwrap();
    assert_eq!(amounts(&balances), ["59570010.146444835"]);
    assert_eq!(*lite_log.lock().unwrap(), ["/"]);
    assert_eq!(*testnet_log.lock().unwrap(), ["/v2/getMasterchainInfo"]);
    assert_eq!(busy_log.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn custom_tokens_and_foreign_addresses_are_refused() {
    let url = node(Log::default(), |path, _| tonapi(path, true)).await;
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
