#![allow(clippy::unwrap_used)]

mod common;

use std::sync::Arc;

use bahaa_chain::ChainError;
use bahaa_tron::TronChain;
use bahaa_types::{Address, ChainFamily};
use common::{
    GENESIS, Log, NILE, OWNER, OWNER_HEX, USDT, USDT_HEX, assets, count, node, tron, tron_on,
};
use serde_json::{Value, json};

const CUSTOM: &str = "TPjjvMwjPoDC32V2dGDYTkLH4E5LAtBZ6C";
const CUSTOM_HEX: &str = "419705bf55c3dcc6d277ebb8fe2a68762268822ba2";
const FRESH_HEX: &str = "410c96c0e10bb5e7e7edacf82ad00e3d60790351da";
const QUIET_HEX: &str = "41000000000000000000000000000000000000beef";

fn word(value: u128) -> String {
    format!("{value:064x}")
}

fn constant(value: u128) -> Value {
    json!({
        "result": {"result": true},
        "energy_used": 900,
        "constant_result": [word(value)],
        "transaction": {"ret": [{}]}
    })
}

fn ledger(genesis: &'static str) -> impl Fn(&str, &Value) -> (u16, Value) + Send + Sync + 'static {
    move |path, body| match path {
        "/wallet/getblockbynum" if body["num"] == 0 => (200, json!({"blockID": genesis})),
        "/wallet/getaccount" => (
            200,
            match body["address"].as_str().unwrap() {
                OWNER_HEX => json!({"address": OWNER_HEX, "balance": 1_500_000, "create_time": 1}),
                QUIET_HEX => json!({"address": QUIET_HEX, "create_time": 1}),
                _ => json!({}),
            },
        ),
        "/wallet/triggerconstantcontract" => {
            let held = |owner: &str| u128::from(owner == OWNER_HEX);
            let parameter = body["parameter"].as_str().unwrap();
            let owner = body["owner_address"].as_str().unwrap();
            let asks_owner = parameter == format!("{:0>64}", &owner[2..]);
            let reply = match (
                body["contract_address"].as_str().unwrap(),
                body["function_selector"].as_str().unwrap(),
            ) {
                (USDT_HEX, "balanceOf(address)") if asks_owner => constant(held(owner) * 2_500_000),
                (CUSTOM_HEX, "decimals()") if parameter.is_empty() => constant(18),
                (CUSTOM_HEX, "balanceOf(address)") if asks_owner => {
                    constant(held(owner) * 3_000_000_000_000_000_000)
                }
                _ => {
                    json!({"result": {"code": "CONTRACT_VALIDATE_ERROR", "message": "536d61727420636f6e7472616374206973206e6f742065786973742e"}})
                }
            };
            (200, reply)
        }
        _ => (405, json!("<html>405</html>")),
    }
}

fn amounts(balances: &[bahaa_chain::Balance]) -> Vec<String> {
    balances
        .iter()
        .map(|balance| format!("{} {}", balance.asset, balance.amount))
        .collect()
}

#[tokio::test]
async fn reads_trx_known_and_custom_token_balances() {
    let log = Log::default();
    let chain = TronChain::new(tron_on(&[node(Arc::clone(&log), ledger(GENESIS)).await])).unwrap();
    let wanted = assets(&["tron", &format!("tron/{USDT}"), &format!("tron/{CUSTOM}")]);
    let balances = chain.balances(&tron(OWNER), &wanted).await.unwrap();
    assert_eq!(
        amounts(&balances),
        [
            "tron 1.5".to_owned(),
            format!("tron/{USDT} 2.5"),
            format!("tron/{CUSTOM} 3"),
        ]
    );
    assert_eq!(
        *log.lock().unwrap(),
        [
            "/wallet/getblockbynum",
            "/wallet/getaccount",
            "/wallet/triggerconstantcontract",
            "/wallet/triggerconstantcontract",
            "/wallet/triggerconstantcontract",
        ]
    );
    chain.balances(&tron(OWNER), &wanted).await.unwrap();
    assert_eq!(count(&log, "/wallet/getblockbynum"), 1);
    assert_eq!(count(&log, "/wallet/triggerconstantcontract"), 5);
}

#[tokio::test]
async fn accounts_that_were_never_activated_or_hold_nothing_read_as_zero() {
    let chain = TronChain::new(tron_on(&[node(Log::default(), ledger(GENESIS)).await])).unwrap();
    let wanted = assets(&["tron", &format!("tron/{USDT}")]);
    for hex in [FRESH_HEX, QUIET_HEX] {
        let address = Address::Tron(bytes(hex));
        let balances = chain.balances(&address, &wanted).await.unwrap();
        assert_eq!(
            amounts(&balances),
            ["tron 0".to_owned(), format!("tron/{USDT} 0")],
            "{address}"
        );
    }
}

#[tokio::test]
async fn skips_an_endpoint_that_serves_another_tron_network() {
    let (nile_log, main_log) = (Log::default(), Log::default());
    let nile = node(Arc::clone(&nile_log), ledger(NILE)).await;
    let main = node(Arc::clone(&main_log), ledger(GENESIS)).await;
    let chain = TronChain::new(tron_on(&[nile, main])).unwrap();
    for _ in 0..2 {
        let balances = chain
            .balances(&tron(OWNER), &assets(&["tron"]))
            .await
            .unwrap();
        assert_eq!(amounts(&balances), ["tron 1.5"]);
    }
    assert_eq!(*nile_log.lock().unwrap(), ["/wallet/getblockbynum"]);
    assert_eq!(count(&main_log, "/wallet/getaccount"), 2);
}

#[tokio::test]
async fn moves_on_when_rate_limited_and_skips_paths_an_endpoint_lacks() {
    let (busy_log, partial_log, full_log) = (Log::default(), Log::default(), Log::default());
    let limited_log = Log::default();
    let full_ledger = ledger(GENESIS);
    let busy = node(Arc::clone(&busy_log), move |path, body| match path {
        "/wallet/getblockbynum" => full_ledger(path, body),
        _ => (429, json!({"Error": "too many requests"})),
    })
    .await;
    let limited_ledger = ledger(GENESIS);
    let limited = node(Arc::clone(&limited_log), move |path, body| match path {
        "/wallet/getblockbynum" => limited_ledger(path, body),
        _ => (
            403,
            json!({"Error": "The key exceeds the frequency limit(3), and the query server is suspended"}),
        ),
    })
    .await;
    let partial_ledger = ledger(GENESIS);
    let partial = node(Arc::clone(&partial_log), move |path, body| match path {
        "/wallet/getaccount" => (405, json!("<html>405</html>")),
        _ => partial_ledger(path, body),
    })
    .await;
    let full = node(Arc::clone(&full_log), ledger(GENESIS)).await;
    let chain = TronChain::new(tron_on(&[busy, limited, partial, full])).unwrap();
    for _ in 0..2 {
        chain
            .balances(&tron(OWNER), &assets(&["tron"]))
            .await
            .unwrap();
    }
    assert_eq!(count(&busy_log, "/wallet/getaccount"), 1);
    assert_eq!(count(&limited_log, "/wallet/getaccount"), 1);
    assert_eq!(count(&partial_log, "/wallet/getaccount"), 2);
    assert_eq!(count(&full_log, "/wallet/getaccount"), 2);
    chain
        .balances(&tron(OWNER), &assets(&[&format!("tron/{USDT}")]))
        .await
        .unwrap();
    assert_eq!(count(&partial_log, "/wallet/triggerconstantcontract"), 1);
    assert_eq!(count(&full_log, "/wallet/triggerconstantcontract"), 0);
}

#[tokio::test]
async fn untrustworthy_replies_are_errors() {
    let wrong_owner = node(Log::default(), |path, body| match path {
        "/wallet/getblockbynum" => (200, json!({"blockID": GENESIS})),
        _ if body["address"] == OWNER_HEX => (200, json!({"address": QUIET_HEX, "balance": 9})),
        _ => (200, json!({"address": OWNER_HEX, "balance": "lots"})),
    })
    .await;
    let chain = TronChain::new(tron_on(&[wrong_owner])).unwrap();
    for address in [tron(OWNER), Address::Tron(bytes(QUIET_HEX))] {
        assert!(matches!(
            chain.balances(&address, &assets(&["tron"])).await,
            Err(ChainError::Network(_))
        ));
    }
    let refusing_log = Log::default();
    let refusing = node(Arc::clone(&refusing_log), |path, _| match path {
        "/wallet/getblockbynum" => (200, json!({"blockID": GENESIS})),
        _ => (
            200,
            json!({"Error": "class org.tron.core.services.http.JsonFormat$ParseException : INVALID hex String"}),
        ),
    })
    .await;
    let spare_log = Log::default();
    let spare = node(Arc::clone(&spare_log), ledger(GENESIS)).await;
    let chain = TronChain::new(tron_on(&[refusing, spare])).unwrap();
    assert!(matches!(
        chain.balances(&tron(OWNER), &assets(&["tron"])).await,
        Err(ChainError::Rejected(message)) if message.contains("INVALID hex")
    ));
    assert_eq!(*spare_log.lock().unwrap(), Vec::<String>::new());
}

#[tokio::test]
async fn refuses_other_networks_and_contracts_that_are_not_tokens() {
    let log = Log::default();
    let chain = TronChain::new(tron_on(&[node(Arc::clone(&log), ledger(GENESIS)).await])).unwrap();
    let evm = Address::parse(
        ChainFamily::Evm,
        "0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266",
    )
    .unwrap();
    assert!(matches!(
        chain.balances(&evm, &assets(&["tron"])).await,
        Err(ChainError::Invalid(_))
    ));
    assert!(matches!(
        chain.balances(&tron(OWNER), &assets(&["bsc"])).await,
        Err(ChainError::Invalid(_))
    ));
    assert_eq!(*log.lock().unwrap(), Vec::<String>::new());
    let stranger = format!("tron/{}", Address::Tron([7; 20]));
    assert_eq!(
        chain.balances(&tron(OWNER), &assets(&[&stranger])).await,
        Err(ChainError::Invalid("this address is not a token contract"))
    );
}

#[tokio::test]
#[ignore = "talks to the real Tron network"]
async fn live_tron_balances() {
    let registry = bahaa_config::Registry::builtin().unwrap();
    let chain = TronChain::new(registry.get(&"tron".parse().unwrap()).unwrap().clone()).unwrap();
    let busy = tron("TQrY8tryqsYVCYS3MFbtffiPp2ccyn4STm");
    let balances = chain
        .balances(&busy, &assets(&["tron", &format!("tron/{USDT}")]))
        .await
        .unwrap();
    assert!(!balances[0].amount.is_zero(), "{balances:?}");
    assert_eq!(balances[1].amount.decimals(), 6);
}

fn bytes(hex: &str) -> [u8; 20] {
    let mut out = [0u8; 20];
    for (slot, at) in out.iter_mut().zip((2..hex.len()).step_by(2)) {
        *slot = u8::from_str_radix(&hex[at..at + 2], 16).unwrap();
    }
    out
}
