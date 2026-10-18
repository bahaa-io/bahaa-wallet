#![allow(clippy::unwrap_used)]

mod common;

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use bahaa_config::{Feature, Registry};
use bahaa_crypto::SecretString;
use bahaa_engine::{Arrival, Engine, Event};
use common::{HARDHAT, Scratch, TRON_0, TRON_0_HEX, TRON_GENESIS, config};
use serde_json::{Value, json};

const USDT: &str = "TR7NHqjeKQxGTCi8q8ZY4pL8otSzgjLj6t";
const USDT_HEX: &str = "a614f803b6fd780986a42c78ec9c7f77e6ded13c";
const TRANSFER: &str = "ddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef";
const SENDER: &str = "000000000000000000000000421f99b9529484b7e863e1fd0399194f85a1cf9e";

fn now_ms() -> u64 {
    u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis(),
    )
    .unwrap()
}

fn answer(path: &str, body: &Value, arrived: u64) -> Value {
    let id = "d4".repeat(32);
    let listed = |data: Value| json!({"data": data, "success": true, "meta": {"at": now_ms()}});
    if path.starts_with(&format!("/v1/accounts/{TRON_0}/transactions/trc20?")) {
        return listed(json!([{"transaction_id": id, "block_timestamp": arrived}]));
    }
    if path.starts_with(&format!("/v1/accounts/{TRON_0}/transactions?")) {
        return listed(json!([]));
    }
    match path {
        "/wallet/getblockbynum" => json!({"blockID": TRON_GENESIS}),
        "/wallet/getaccount" => json!({"address": TRON_0_HEX, "balance": 3_000_000}),
        "/wallet/triggerconstantcontract" => json!({
            "result": {"result": true},
            "constant_result": [format!("{:064x}", 7_500_000)],
            "transaction": {"ret": [{}]}
        }),
        "/wallet/gettransactioninfobyid" if body["value"] == id.as_str() => json!({
            "id": id,
            "blockNumber": 87_200_000,
            "blockTimeStamp": arrived,
            "receipt": {"result": "SUCCESS"},
            "log": [{
                "address": USDT_HEX,
                "topics": [TRANSFER, SENDER, format!("{:0>64}", &TRON_0_HEX[2..])],
                "data": format!("{:064x}", 7_500_000)
            }]
        }),
        _ => json!({}),
    }
}

#[tokio::test]
async fn incoming_tron_tokens_join_the_wallets_history() {
    let scratch = Scratch::new();
    let arrived = now_ms() - 60 * 60 * 1000;
    let url = common::rest_node(move |path, body| answer(path, body, arrived)).await;
    let mut tron = common::tron_network(url);
    tron.endpoints[0].features = vec![Feature::History];
    let registry = Registry::with_networks(vec![tron]).unwrap();
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
    assert_eq!(fresh.len(), 1);
    assert_eq!(fresh[0].cautions, []);
    let entry = &fresh[0].received;
    assert_eq!(entry.to.to_string(), TRON_0);
    assert_eq!(entry.asset, format!("tron/{USDT}").parse().unwrap());
    assert_eq!(entry.amount.to_string(), "7.5");
    assert_eq!((entry.block, entry.time), (87_200_000, arrived / 1000));
    let incoming = engine.incoming().await.unwrap();
    assert_eq!(incoming.transfers, fresh);
    let coverage = &incoming.coverage[0];
    assert_eq!(coverage.network.as_str(), "tron");
    assert_eq!(coverage.address.to_string(), TRON_0);
    let month = now_ms() / 1000 - 30 * 24 * 60 * 60;
    assert!(coverage.complete_since.abs_diff(month) < 60, "{coverage:?}");
}
