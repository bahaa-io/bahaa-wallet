#![allow(clippy::unwrap_used)]

mod common;

use std::sync::{Arc, Mutex};

use bahaa_chain::{Chain, ChainError, FeeItem, FeeQuote, Transfer, TxStatus, check_signed};
use bahaa_solana::SolanaChain;
use bahaa_types::{Amount, AssetId};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use common::{
    AAPLX, BERN, FRIEND, Ledger, Log, OWNER, TOKEN, TOKEN_2022, USDC, address, called, edited_mint,
    native, node, seed, solana_on, token_account, token_account_with, usdc, wallet,
};
use serde_json::{Value, json};

const OWNER_USDC: &str = "5N3f1tj9v1vc5TUZ8S7mCAnVmjVKrfnzXWhxLaxyZAgt";
const OWNER_AAPLX: &str = "3s4MfAv21S88M2Lsq9bwKHgd6TnA2FcXk13vRD7FvLEj";
const FRIEND_AAPLX: &str = "7S4gcRsorTTSv94qX3vLwRwKKGiRMhp6cR8EyJa6Xdy7";
const OWNER_BERN: &str = "29g64q6UDASAkMchmVW688eM2oRkm5sXb7JmgVPNQR4k";
const IMMUTABLE: (u16, &[u8]) = (7, &[]);

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

fn token(mint: &str) -> AssetId {
    format!("solana/{mint}").parse().unwrap()
}

fn holds(signed: &[u8], data: &[u8]) -> bool {
    signed.windows(data.len()).any(|window| window == data)
}

fn instruction(head: &[u8], units: u64, decimals: u8, fee: Option<u64>) -> Vec<u8> {
    let mut data = head.to_vec();
    data.extend_from_slice(&units.to_le_bytes());
    data.push(decimals);
    if let Some(fee) = fee {
        data.extend_from_slice(&fee.to_le_bytes());
    }
    data
}

fn with_bern(units: u64) -> Ledger {
    let mut tokens = ledger(1_000_000_000);
    tokens.units = 30_000;
    tokens.epoch = 900;
    tokens.accounts.insert(
        OWNER_BERN.to_owned(),
        token_account_with(TOKEN_2022, OWNER, BERN, units, &[IMMUTABLE, (2, &[0; 8])]),
    );
    tokens
}

fn with_aaplx(units: u64) -> Ledger {
    let mut tokens = ledger(1_000_000_000);
    tokens.units = 30_000;
    tokens.accounts.insert(
        OWNER_AAPLX.to_owned(),
        token_account_with(TOKEN_2022, OWNER, AAPLX, units, &[IMMUTABLE]),
    );
    tokens
}

fn extension(data: &[u8], kind: u16) -> usize {
    let mut at = 166;
    loop {
        let found = u16::from_le_bytes([data[at], data[at + 1]]);
        let len = usize::from(u16::from_le_bytes([data[at + 2], data[at + 3]]));
        if found == kind {
            return at + 4;
        }
        at += 4 + len;
    }
}

#[tokio::test]
async fn a_token_2022_fee_is_named_signed_and_kept_from_what_arrives() {
    let (chain, log) = connect(with_bern(5_000_000)).await;
    let sent = transfer(token(BERN), "10", 5);
    let (plan, fee) = chain.prepare(&sent).await.unwrap();
    assert_eq!(
        lines(&fee),
        [
            (FeeItem::Signature, "0.000005".to_owned()),
            (
                FeeItem::Priority { units: 36_000 },
                "0.000000036".to_owned()
            ),
            (FeeItem::Activation, "0.0015748".to_owned()),
        ]
    );
    assert_eq!(fee.withheld().unwrap().to_string(), "0.269");
    let signed = chain.sign(&plan, &seed()).unwrap();
    assert!(holds(
        &signed.bytes,
        &instruction(&[26, 1], 1_000_000, 5, Some(26_900))
    ));
    let decoded = chain.decode(&signed).unwrap();
    check_signed(&sent, &decoded).unwrap();
    assert_eq!(decoded.asset, token(BERN));
    assert_eq!(called(&log, "getEpochInfo").len(), 1);
    let mut all = transfer(token(BERN), "0", 5);
    all.max = true;
    let (plan, fee) = chain.prepare(&all).await.unwrap();
    assert_eq!(chain.planned(&plan).amount.to_string(), "50");
    assert_eq!(fee.withheld().unwrap().to_string(), "1.345");
}

#[tokio::test]
async fn scaled_tokens_send_the_raw_amount_that_shows_as_typed() {
    let (chain, log) = connect(with_aaplx(187_360)).await;
    let sent = transfer(token(AAPLX), "0.001", 8);
    let (plan, fee) = chain.prepare(&sent).await.unwrap();
    assert_eq!(fee.withheld(), None);
    assert_eq!(fee.lines().last().unwrap().amount.to_string(), "0.00155956");
    let signed = chain.sign(&plan, &seed()).unwrap();
    assert!(holds(&signed.bytes, &instruction(&[12], 99_622, 8, None)));
    let decoded = chain.decode(&signed).unwrap();
    check_signed(&sent, &decoded).unwrap();
    assert_eq!(decoded.amount.to_string(), "0.001");
    assert_eq!(called(&log, "getEpochInfo"), Vec::<Value>::new());
    let mut all = transfer(token(AAPLX), "0", 8);
    all.max = true;
    let (plan, _) = chain.prepare(&all).await.unwrap();
    assert_eq!(chain.planned(&plan).amount.to_string(), "0.00188072");
    let too_much = chain
        .prepare(&transfer(token(AAPLX), "0.002", 8))
        .await
        .unwrap_err();
    let ChainError::Insufficient {
        needed, available, ..
    } = too_much
    else {
        panic!("{too_much}");
    };
    assert_eq!(
        (needed.to_string(), available.to_string()),
        ("0.002".to_owned(), "0.00188072".to_owned())
    );
}

async fn refused(ledger: Ledger, memo: Option<&str>) -> Option<String> {
    let (chain, log) = connect(ledger).await;
    let mut sent = transfer(token(AAPLX), "0.001", 8);
    sent.memo = memo.map(str::to_owned);
    let outcome = chain.prepare(&sent).await;
    if outcome.is_err() {
        assert_eq!(called(&log, "simulateTransaction"), Vec::<Value>::new());
    }
    outcome.err().map(|err| err.to_string())
}

#[tokio::test]
async fn token_2022_transfers_that_would_fail_are_refused_with_the_reason() {
    let mut hooked = with_aaplx(187_360);
    hooked.accounts.insert(
        AAPLX.to_owned(),
        edited_mint("AAPLx", |data| {
            let at = extension(data, 14);
            data[at + 32..at + 64].copy_from_slice(&[9; 32]);
        }),
    );
    assert!(refused(hooked, None).await.unwrap().contains("own program"));
    let mut paused = with_aaplx(187_360);
    paused.accounts.insert(
        AAPLX.to_owned(),
        edited_mint("AAPLx", |data| {
            let at = extension(data, 26);
            data[at + 32] = 1;
        }),
    );
    assert!(refused(paused, None).await.unwrap().contains("paused"));
    let mut frozen = with_aaplx(187_360);
    let mut account = token_account_with(TOKEN_2022, OWNER, AAPLX, 187_360, &[IMMUTABLE]);
    let mut data = STANDARD
        .decode(account["data"][0].as_str().unwrap())
        .unwrap();
    data[108] = 2;
    account["data"] = json!([STANDARD.encode(&data), "base64"]);
    frozen.accounts.insert(OWNER_AAPLX.to_owned(), account);
    assert!(refused(frozen, None).await.unwrap().contains("froze"));
    let mut picky = with_aaplx(187_360);
    picky.accounts.insert(
        FRIEND_AAPLX.to_owned(),
        token_account_with(TOKEN_2022, FRIEND, AAPLX, 0, &[IMMUTABLE, (8, &[1])]),
    );
    assert!(refused(picky.clone(), None).await.unwrap().contains("memo"));
    assert_eq!(refused(picky, Some("order 7")).await, None);
}
