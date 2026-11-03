#![allow(clippy::unwrap_used)]

mod common;

use std::sync::{Arc, Mutex};

use bahaa_chain::{Chain, ChainError, FeeItem, FeeQuote, Transfer, TxStatus, check_signed};
use bahaa_solana::SolanaChain;
use bahaa_types::{Amount, AssetId};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use common::{
    FRIEND, Ledger, Log, OWNER, TOKEN, USDC, address, called, native, node, seed, solana_on,
    token_account, usdc, wallet,
};
use serde_json::{Value, json};

const OWNER_USDC: &str = "5N3f1tj9v1vc5TUZ8S7mCAnVmjVKrfnzXWhxLaxyZAgt";

fn ledger(owner: u64) -> Ledger {
    let mut ledger = Ledger {
        fees: vec![0, 0, 0, 1_000],
        units: 450,
        ..Ledger::default()
    };
    ledger.accounts.insert(OWNER.to_owned(), wallet(owner));
    ledger
        .accounts
        .insert(FRIEND.to_owned(), wallet(500_000_000));
    ledger
}

async fn connect(ledger: Ledger) -> (SolanaChain, Log) {
    let log = Arc::new(Mutex::new(Vec::new()));
    let url = node(Arc::clone(&log), ledger).await;
    (SolanaChain::new(solana_on(&[url])).unwrap(), log)
}

fn lines(fee: &FeeQuote) -> Vec<(FeeItem, String)> {
    fee.lines()
        .iter()
        .map(|line| (line.item.clone(), line.amount.to_string()))
        .collect()
}

fn transfer(asset: AssetId, amount: &str, decimals: u8) -> Transfer {
    Transfer {
        account: 0,
        from: address(OWNER),
        to: address(FRIEND),
        asset,
        amount: Amount::parse(amount, decimals).unwrap(),
        max: false,
        memo: None,
    }
}

#[tokio::test]
async fn a_coin_transfer_pays_its_signature_and_a_priority_fee_and_signs_as_quoted() {
    let (chain, log) = connect(ledger(1_000_000_000)).await;
    let (plan, fee) = chain.prepare(&transfer(native(), "0.1", 9)).await.unwrap();
    assert_eq!(
        lines(&fee),
        [
            (FeeItem::Signature, "0.000005".to_owned()),
            (FeeItem::Priority { units: 540 }, "0.000000001".to_owned()),
        ]
    );
    assert_eq!(fee.max(), fee.expected());
    let simulated = &called(&log, "simulateTransaction")[0][1];
    assert_eq!(simulated["sigVerify"], false);
    assert_eq!(simulated["replaceRecentBlockhash"], true);
    let signed = chain.sign(&plan, &seed()).unwrap();
    let decoded = chain.decode(&signed).unwrap();
    check_signed(chain.planned(&plan), &decoded).unwrap();
    assert_eq!(decoded.to, address(FRIEND));
    assert_eq!(decoded.amount.to_string(), "0.1");
    assert!(signed.bytes.len() < 1232);
}

#[tokio::test]
async fn max_sends_everything_but_the_fee() {
    let (chain, _) = connect(ledger(1_000_000_000)).await;
    let mut all = transfer(native(), "0", 9);
    all.max = true;
    let (plan, fee) = chain.prepare(&all).await.unwrap();
    assert_eq!(fee.expected().to_string(), "0.000005001");
    assert_eq!(chain.planned(&plan).amount.to_string(), "0.999994999");
    let decoded = chain.decode(&chain.sign(&plan, &seed()).unwrap()).unwrap();
    assert_eq!(decoded.amount.to_string(), "0.999994999");
}

#[tokio::test]
async fn the_network_minimum_balance_is_respected_on_both_ends() {
    let mut fresh = ledger(1_000_000_000);
    fresh.accounts.remove(FRIEND);
    let (chain, _) = connect(fresh).await;
    let refused = chain
        .prepare(&transfer(native(), "0.0001", 9))
        .await
        .unwrap_err();
    assert!(refused.to_string().contains("first deposit"), "{refused}");
    assert!(chain.prepare(&transfer(native(), "0.001", 9)).await.is_ok());
    let (chain, _) = connect(ledger(1_000_000)).await;
    let dust = chain
        .prepare(&transfer(native(), "0.0005", 9))
        .await
        .unwrap_err();
    assert!(dust.to_string().contains("must keep"), "{dust}");
    let mut all = transfer(native(), "0", 9);
    all.max = true;
    assert!(chain.prepare(&all).await.is_ok());
    let short = chain
        .prepare(&transfer(native(), "0.001", 9))
        .await
        .unwrap_err();
    assert!(matches!(short, ChainError::Insufficient { .. }), "{short}");
}

#[tokio::test]
async fn a_token_transfer_opens_the_recipients_account_and_says_so() {
    let mut tokens = ledger(1_000_000_000);
    tokens.units = 30_000;
    tokens
        .accounts
        .insert(OWNER_USDC.to_owned(), token_account(OWNER, USDC, 8_509_335));
    let (chain, _) = connect(tokens).await;
    let mut sent = transfer(usdc(), "2.5", 6);
    sent.memo = Some("invoice 12".to_owned());
    let (plan, fee) = chain.prepare(&sent).await.unwrap();
    assert_eq!(
        lines(&fee),
        [
            (FeeItem::Signature, "0.000005".to_owned()),
            (
                FeeItem::Priority { units: 36_000 },
                "0.000000036".to_owned()
            ),
            (FeeItem::Activation, "0.00148844".to_owned()),
        ]
    );
    let decoded = chain.decode(&chain.sign(&plan, &seed()).unwrap()).unwrap();
    check_signed(&sent, &decoded).unwrap();
    assert_eq!(decoded.asset, usdc());
    assert_eq!(decoded.memo.as_deref(), Some("invoice 12"));
    let too_much = chain.prepare(&transfer(usdc(), "9", 6)).await.unwrap_err();
    let ChainError::Insufficient { symbol, .. } = too_much else {
        panic!("{too_much}");
    };
    assert_eq!(symbol, "USDC");
}

#[tokio::test]
async fn token_accounts_and_programs_are_not_recipients() {
    let mut odd = ledger(1_000_000_000);
    odd.accounts
        .insert(OWNER_USDC.to_owned(), token_account(OWNER, USDC, 1));
    odd.accounts.insert(
        TOKEN.to_owned(),
        json!({"lamports": 1, "owner": "BPFLoader2111111111111111111111111111111111", "executable": true, "data": ["", "base64"], "space": 0}),
    );
    let (chain, log) = connect(odd).await;
    for (to, needle) in [(OWNER_USDC, "wallet's own address"), (TOKEN, "program")] {
        let mut sent = transfer(native(), "0.01", 9);
        sent.to = address(to);
        let refused = chain.prepare(&sent).await.unwrap_err();
        assert!(refused.to_string().contains(needle), "{refused}");
    }
    assert_eq!(called(&log, "simulateTransaction"), Vec::<Value>::new());
}

#[tokio::test]
async fn a_failing_simulation_refuses_the_quote() {
    let mut failing = ledger(1_000_000_000);
    failing.simulation_error = Some(json!({"InstructionError": [2, {"Custom": 1}]}));
    let (chain, _) = connect(failing).await;
    let refused = chain
        .prepare(&transfer(native(), "0.1", 9))
        .await
        .unwrap_err();
    let ChainError::Rejected(reason) = refused else {
        panic!("{refused}");
    };
    assert!(reason.contains("InstructionError"), "{reason}");
}

#[tokio::test]
async fn a_sent_transfer_is_followed_until_it_is_confirmed() {
    let (chain, _) = connect(ledger(1_000_000_000)).await;
    let (plan, _) = chain.prepare(&transfer(native(), "0.1", 9)).await.unwrap();
    let signed = chain.sign(&plan, &seed()).unwrap();
    let id = signed.id.as_str();
    let mut sending = ledger(1_000_000_000);
    sending.sent = Some(json!(id));
    let states = [
        (Value::Null, Value::Null),
        (
            json!({"slot": 42, "err": null, "confirmationStatus": "processed"}),
            Value::Null,
        ),
        (
            json!({"slot": 42, "err": null, "confirmationStatus": "confirmed"}),
            json!({"slot": 42, "meta": {"fee": 5_001, "err": null}, "transaction": [STANDARD.encode(&signed.bytes), "base64"]}),
        ),
        (
            json!({"slot": 42, "err": {"InstructionError": [3, "InvalidArgument"]}, "confirmationStatus": "finalized"}),
            Value::Null,
        ),
    ];
    let mut seen = Vec::new();
    for (status, found) in states {
        let mut state = sending.clone();
        state.status = status;
        state.found = found;
        let (chain, _) = connect(state).await;
        assert_eq!(chain.broadcast(&signed).await.unwrap(), signed.id);
        seen.push(
            chain
                .status(&signed.id, &address(OWNER), None)
                .await
                .unwrap(),
        );
    }
    assert_eq!(seen[0], TxStatus::NotFound);
    assert_eq!(seen[1], TxStatus::Pending);
    assert_eq!(
        seen[2],
        TxStatus::Confirmed {
            block: Some(42),
            fee: Some(Amount::parse("0.000005001", 9).unwrap()),
            transaction: Some(signed.id.clone()),
        }
    );
    assert!(matches!(&seen[3], TxStatus::Failed(reason) if reason.contains("InvalidArgument")));
    let mut other = sending;
    other.sent = Some(json!("1111"));
    let (chain, _) = connect(other).await;
    assert!(chain.broadcast(&signed).await.is_err());
}
