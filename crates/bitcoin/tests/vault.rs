#![allow(clippy::unwrap_used)]

mod common;

use std::sync::{Arc, Mutex};

use bahaa_bitcoin::BitcoinChain;
use bahaa_chain::{Call, Chain, ChainError, ContractSwap, check_call};
use bahaa_testkit::electrum::{Ledger, Log};
use bahaa_types::{Amount, hex};
use common::{FIRST, FIRST_CHANGE, GENESIS, SECOND, address, bitcoin_on, native, seed};

const VAULT: &str = "bc1pske0hr4ne5xajxdd6gz25m6sdsy6xva0aly9phgl0xy0frj9rl0qc94fxv";
const PAYLOAD: &str = "01039858effd232b4033e47d90003d41ec34ecaeda94640033621d090000000000000000000000003201000200000000";

async fn started() -> BitcoinChain {
    let mut ledger = Ledger::new(GENESIS);
    ledger.fund(&address(SECOND), 30_000, 1);
    ledger.fund(&address(FIRST_CHANGE), 20_000, 2);
    let url = bahaa_testkit::electrum::serve(Log::default(), Arc::new(Mutex::new(ledger))).await;
    let chain = BitcoinChain::new(bitcoin_on(&url)).unwrap();
    chain.derive(&seed(), 0).unwrap();
    chain
}

fn swap(amount: &str) -> ContractSwap {
    let amount = Amount::parse(amount, 8).unwrap();
    ContractSwap {
        account: 0,
        from: address(FIRST),
        call: Call {
            to: address(VAULT),
            value: amount,
            data: hex::decode(PAYLOAD).unwrap(),
        },
        spender: None,
        sell: native(),
        amount,
        buy: None,
        ours: true,
        spent: None,
    }
}

#[tokio::test]
async fn a_vault_deposit_keeps_the_order_chainflip_reads_and_signs_like_a_send() {
    let chain = started().await;
    let (plans, fee) = chain.prepare_swap(&swap("0.0003")).await.unwrap();
    assert_eq!(plans.len(), 1);
    assert!(fee.expected() > Amount::parse("0", 8).unwrap());
    let signed = chain.sign_call(&plans[0], &seed()).unwrap();
    let decoded = chain.decode_call(&signed).unwrap();
    check_call(plans[0].planned(), &decoded).unwrap();
    assert_eq!(decoded.from, address(FIRST));
    assert_eq!(decoded.call.to, address(VAULT));
    assert_eq!(decoded.call.value.to_string(), "0.0003");
    assert!(
        chain.decode(&signed).is_err(),
        "a vault deposit is not read as a plain transfer"
    );
    let text = hex::encode(&signed.bytes);
    let vault_at = text.find("5120").unwrap();
    let data_at = text.find(&format!("6a30{PAYLOAD}")).unwrap();
    let refund_at = data_at + 4 + PAYLOAD.len() + 16 + 2;
    assert!(vault_at < data_at, "the vault deposit comes first");
    assert_eq!(
        &text[refund_at..refund_at + 4],
        "0014",
        "a refund output follows the data"
    );
}

#[tokio::test]
async fn a_vault_deposit_always_leaves_a_refund_output() {
    let chain = started().await;
    let everything = chain.prepare_swap(&swap("0.0005")).await.unwrap_err();
    assert!(
        matches!(&everything, ChainError::Insufficient { needed, .. } if needed.units() > Amount::parse("0.0005", 8).unwrap().units()),
        "{everything}"
    );
    let mut long = swap("0.0001");
    long.call.data = vec![1; 81];
    assert!(chain.prepare_swap(&long).await.is_err());
    let mut other = swap("0.0001");
    other.call.value = Amount::parse("0.0002", 8).unwrap();
    assert!(chain.prepare_swap(&other).await.is_err());
    let mut home = swap("0.0001");
    home.call.to = address(FIRST_CHANGE);
    assert!(chain.prepare_swap(&home).await.is_err());
}
