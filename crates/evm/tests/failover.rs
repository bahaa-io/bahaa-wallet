#![allow(clippy::unwrap_used)]

mod common;

use std::sync::Arc;

use bahaa_chain::{Chain, TxId, TxStatus};
use bahaa_evm::EvmChain;
use common::{Log, assets, bsc_on, node, owner, result};
use serde_json::{Value, json};

const NO_RECEIPTS: &str = "the method eth_getTransactionReceipt does not exist/is not available";

fn receipt() -> Value {
    json!({"status": "0x1", "blockNumber": "0x10", "gasUsed": "0x5208", "effectiveGasPrice": "0x3b9aca00"})
}

fn serves(receipts: bool) -> impl Fn(&str, &Value) -> (u16, Value) + Send + Sync + 'static {
    move |method, _| match method {
        "eth_chainId" => result(&json!("0x38")),
        "eth_getBalance" => result(&json!("0x1")),
        "eth_getTransactionReceipt" if receipts => result(&receipt()),
        "eth_getTransactionReceipt" => (
            403,
            json!({"jsonrpc": "2.0", "id": 1, "error": {"code": -32601, "message": NO_RECEIPTS}}),
        ),
        _ => (502, json!("bad gateway")),
    }
}

fn count(log: &Log, method: &str) -> usize {
    log.lock()
        .unwrap()
        .iter()
        .filter(|seen| *seen == method)
        .count()
}

#[tokio::test]
async fn an_endpoint_that_refuses_receipts_is_skipped_for_receipts_only() {
    let (picky_log, full_log) = (Log::default(), Log::default());
    let picky = node(Arc::clone(&picky_log), serves(false)).await;
    let full = node(Arc::clone(&full_log), serves(true)).await;
    let chain = EvmChain::new(bsc_on(&[picky, full])).unwrap();
    let id = TxId::new(format!("0x{}", "ab".repeat(32))).unwrap();
    let confirmed = chain.status(&id, &common::owner(), None).await.unwrap();
    assert!(matches!(
        confirmed,
        TxStatus::Confirmed {
            block: Some(16),
            ..
        }
    ));
    assert_eq!(count(&picky_log, "eth_getTransactionReceipt"), 1);
    assert_eq!(count(&full_log, "eth_getTransactionReceipt"), 1);
    chain.balances(&owner(), &assets(&["bsc"])).await.unwrap();
    assert_eq!(count(&picky_log, "eth_getBalance"), 1);
    assert_eq!(count(&full_log, "eth_getBalance"), 0);
    chain.status(&id, &common::owner(), None).await.unwrap();
    assert_eq!(count(&picky_log, "eth_getTransactionReceipt"), 1);
    assert_eq!(count(&full_log, "eth_getTransactionReceipt"), 2);
}

#[tokio::test]
async fn an_http_error_without_a_capability_reason_still_cools_the_endpoint_down() {
    let (broken_log, full_log) = (Log::default(), Log::default());
    let broken = node(Arc::clone(&broken_log), |method, _| match method {
        "eth_chainId" => result(&json!("0x38")),
        _ => (
            401,
            json!({"jsonrpc": "2.0", "id": 1, "error": {"code": -32000, "message": "tenant disabled"}}),
        ),
    })
    .await;
    let full = node(Arc::clone(&full_log), serves(true)).await;
    let chain = EvmChain::new(bsc_on(&[broken, full])).unwrap();
    chain.balances(&owner(), &assets(&["bsc"])).await.unwrap();
    chain.balances(&owner(), &assets(&["bsc"])).await.unwrap();
    assert_eq!(count(&broken_log, "eth_getBalance"), 1);
    assert_eq!(count(&full_log, "eth_getBalance"), 2);
}

#[tokio::test]
#[ignore = "talks to the real BSC network"]
async fn live_bsc_status_of_a_real_transfer() {
    let registry = bahaa_config::Registry::builtin().unwrap();
    let chain = EvmChain::new(registry.get(&"bsc".parse().unwrap()).unwrap().clone()).unwrap();
    let id =
        TxId::new("0xf54187da477e131a07472915a7fd23896829b69ce8ad6f09f42e6690814fbbfe").unwrap();
    let status = chain.status(&id, &common::owner(), None).await.unwrap();
    let TxStatus::Confirmed {
        block,
        fee,
        transaction,
    } = status
    else {
        panic!("expected a confirmed transfer, got {status:?}");
    };
    assert_eq!(block, Some(0x07a4_d222));
    assert_eq!(transaction, Some(id));
    assert!(fee.is_some_and(|fee| !fee.is_zero()));
}

fn relays(answer: Option<&'static str>) -> impl Fn(&str, &Value) -> (u16, Value) + Send + Sync {
    move |method, _| match (method, answer) {
        ("eth_chainId", _) => result(&json!("0x38")),
        ("eth_sendRawTransaction", None) => result(&json!(format!("0x{}", "cd".repeat(32)))),
        ("eth_sendRawTransaction", Some(message)) => (
            200,
            json!({"jsonrpc": "2.0", "id": 1, "error": {"code": -32000, "message": message}}),
        ),
        _ => (502, json!("bad gateway")),
    }
}

#[tokio::test]
async fn a_broadcast_reaches_every_endpoint_and_one_acceptance_is_enough() {
    let signed = bahaa_chain::Signed {
        id: TxId::new(format!("0x{}", "cd".repeat(32))).unwrap(),
        bytes: vec![0x02, 0xf8, 0x01],
    };
    let cases = [
        (None, Some("already known"), true),
        (Some("nonce too low"), None, true),
        (Some("already known"), Some("already known"), true),
        (Some("nonce too low"), Some("insufficient funds"), false),
    ];
    for (first, second, accepted) in cases {
        let (one, two) = (Log::default(), Log::default());
        let urls = [
            node(Arc::clone(&one), relays(first)).await,
            node(Arc::clone(&two), relays(second)).await,
        ];
        let chain = EvmChain::new(bsc_on(&urls)).unwrap();
        let outcome = chain.broadcast(&signed).await;
        assert_eq!(
            outcome.is_ok(),
            accepted,
            "{first:?} {second:?}: {outcome:?}"
        );
        assert_eq!(count(&one, "eth_sendRawTransaction"), 1);
        assert_eq!(count(&two, "eth_sendRawTransaction"), 1);
    }
}
