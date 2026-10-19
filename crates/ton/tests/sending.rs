#![allow(clippy::unwrap_used)]

mod common;

use std::sync::Arc;

use bahaa_chain::{Chain, ChainError, Signed, TxId};
use bahaa_config::{Endpoint, Network, Protocol, Registry};
use bahaa_ton::TonChain;
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use serde_json::{Value, json};
use url::Url;

use common::{Log, node, unhex};

const ZERO_STATE: &str = "17a3a92992aabea785a7a090985a265cd31f323d849da51239737e321fb05569";
const SENT: &str = "b5ee9c72410205010001100001e5880027fb320fdb12cc1b3721f24b7aa94d605395f66d142e5111e3ff8dfad20c49ca039b4b3b73fffff88b566b9008000000559e4f7c61649cead47b81fc559961f1397ad6d17bf54953722f26dc0e635781c76df6d6f81c372949e1c8cb6a2cd85973bdab865f605980a59459cdf5e17aca0701020a0ec3c86d0302030000016862005bfa00866642a075cac79e914142651bcacae9fd297238395e0f73c0e9af00f3a018685d68000000000000000000000000010400ac0f8a7ea5000000006acd70d7506cc74a400801bca4e3e99d4b7eeed43fa097305639a10a68520be36b0b239c7ece42e7159acf0004ff6641fb62598366e43e496f5529ac0a72becda285ca223c7ff1bf5a41893942027a92ffc8";
const SENT_ID: &str = "88d7201d27158e7cbaf4cceaef8ce46f460e9dc1e084490b2485eb6e6112bd54";
const REFUSAL: &str = "cannot apply external message to current state : External message was not accepted: cannot run message on account: inbound external message rejected by transaction 13FD9907ED89660D9B90F925BD54A6B029CAFB368A172888F1FFC6FD690624E5:\nexitcode=133, steps=49, gas_used=0\nVM Log (truncated):\n...SIGNU";

fn network(endpoints: &[(Protocol, &Url)]) -> Network {
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

fn signed(bytes: Vec<u8>) -> Signed {
    Signed {
        id: TxId::new(SENT_ID.to_owned()).unwrap(),
        bytes,
    }
}

fn rejected() -> ChainError {
    ChainError::Rejected("the wallet contract refused the message with exit code 133".to_owned())
}

fn tonapi_send(path: &str, body: &Value) -> (u16, Value) {
    match path {
        "/liteserver/get_masterchain_info" => (200, json!({"init": {"root_hash": ZERO_STATE}})),
        "/liteserver/send_message" if body["body"] == STANDARD.encode(unhex(SENT)) => {
            (200, json!({"code": 1}))
        }
        "/liteserver/send_message" => (
            406,
            json!({"error": format!("lite server error, code -701: {REFUSAL}")}),
        ),
        _ => (404, json!({"error": "route not found"})),
    }
}

fn center_send(path: &str, body: &Value, ready: bool) -> (u16, Value) {
    match path {
        "/v2/getMasterchainInfo" => (
            200,
            json!({"ok": true, "result": {"init": {"root_hash": STANDARD.encode(unhex(ZERO_STATE))}}}),
        ),
        "/v2/sendBoc" if !ready => (
            500,
            json!({"ok": false, "error": "LITE_SERVER_NOTREADY: timeout", "code": 500}),
        ),
        "/v2/sendBoc" if body["boc"] == STANDARD.encode(unhex(SENT)) => {
            (200, json!({"ok": true, "result": {"@type": "ok"}}))
        }
        "/v2/sendBoc" => (
            500,
            json!({"ok": false, "error": format!("LITE_SERVER_UNKNOWN: {REFUSAL}"), "code": 500}),
        ),
        _ => (404, json!({"ok": false, "error": "not found"})),
    }
}

#[tokio::test]
async fn tonapi_and_ton_center_hand_transfers_over_and_name_refusals() {
    let tonapi = node(Log::default(), tonapi_send).await;
    let center = node(Log::default(), |path, body| center_send(path, body, true)).await;
    for (protocol, url) in [(Protocol::TonApi, &tonapi), (Protocol::TonCenter, &center)] {
        let chain = TonChain::new(network(&[(protocol, url)])).unwrap();
        assert_eq!(
            chain.broadcast(&signed(unhex(SENT))).await,
            Ok(TxId::new(SENT_ID.to_owned()).unwrap()),
            "{protocol:?}"
        );
        assert_eq!(
            chain.broadcast(&signed(vec![0xb5, 0xee])).await,
            Err(rejected()),
            "{protocol:?}"
        );
    }
    let log = Log::default();
    let busy = node(Arc::clone(&log), |path, body| {
        center_send(path, body, false)
    })
    .await;
    let chain = TonChain::new(network(&[
        (Protocol::TonCenter, &busy),
        (Protocol::TonApi, &tonapi),
    ]))
    .unwrap();
    assert!(chain.broadcast(&signed(unhex(SENT))).await.is_ok());
    assert!(log.lock().unwrap().contains(&"/v2/sendBoc".to_owned()));
}

#[tokio::test]
async fn a_transfer_is_handed_to_every_endpoint() {
    let (api_log, center_log) = (Log::default(), Log::default());
    let tonapi = node(Arc::clone(&api_log), tonapi_send).await;
    let center = node(Arc::clone(&center_log), |path, body| {
        center_send(path, body, true)
    })
    .await;
    let chain = TonChain::new(network(&[
        (Protocol::TonApi, &tonapi),
        (Protocol::TonCenter, &center),
    ]))
    .unwrap();
    assert!(chain.broadcast(&signed(unhex(SENT))).await.is_ok());
    let sent = |log: &Log, path: &str| {
        log.lock()
            .unwrap()
            .iter()
            .filter(|seen| *seen == path)
            .count()
    };
    assert_eq!(sent(&api_log, "/liteserver/send_message"), 1);
    assert_eq!(sent(&center_log, "/v2/sendBoc"), 1);
}
