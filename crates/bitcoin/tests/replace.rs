#![allow(clippy::unwrap_used)]

mod common;

use std::sync::{Arc, Mutex};

use bahaa_bitcoin::BitcoinChain;
use bahaa_chain::{Chain, Replace, Signed, Transfer, Wait, check_signed};
use bahaa_testkit::electrum::{Ledger, Log};
use bahaa_types::{Amount, BitcoinType};
use common::{FIRST, FIRST_CHANGE, FRIEND, GENESIS, SECOND, address, bitcoin_on, native, seed};

async fn sent(amount: &str, max: bool) -> (BitcoinChain, Signed) {
    sent_as(
        BitcoinType::NativeSegwit,
        [FIRST, SECOND, FIRST_CHANGE],
        amount,
        max,
    )
    .await
}

async fn sent_as(
    kind: BitcoinType,
    [first, second, change]: [&str; 3],
    amount: &str,
    max: bool,
) -> (BitcoinChain, Signed) {
    let mut ledger = Ledger::new(GENESIS);
    ledger.fund(&address(second), 30_000, 1);
    ledger.fund(&address(change), 20_000, 2);
    let ledger = Arc::new(Mutex::new(ledger));
    let url = bahaa_testkit::electrum::serve(Log::default(), ledger).await;
    let chain = BitcoinChain::new(bitcoin_on(&url)).unwrap();
    chain.derive_as(&seed(), 0, kind).unwrap();
    let transfer = Transfer {
        account: 0,
        from: address(first),
        to: address(FRIEND),
        asset: native(),
        amount: Amount::parse(amount, 8).unwrap(),
        max,
        memo: None,
    };
    let (plan, _) = chain.prepare(&transfer).await.unwrap();
    let signed = chain.sign(&plan, &seed()).unwrap();
    chain.broadcast(&signed).await.unwrap();
    (chain, signed)
}

#[tokio::test]
async fn a_speed_up_pays_more_from_the_change_and_keeps_the_payment() {
    let (chain, original) = sent("0.00025", false).await;
    let (plan, fee) = chain.replace(&original, 0, Replace::SpeedUp).await.unwrap();
    assert_eq!(fee.expected().to_string(), "0.00000423");
    assert_eq!(fee.pace().rate.unwrap().amount.to_string(), "3");
    let before = fee.replaced().unwrap();
    assert_eq!(before.fee.to_string(), "0.00000282");
    assert_eq!(before.pace.rate.unwrap().amount.to_string(), "2");
    assert_eq!(before.pace.wait, Some(Wait::About(1_200)));
    let planned = chain.planned(&plan);
    assert_eq!(planned.to, address(FRIEND));
    assert_eq!(planned.amount.to_string(), "0.00025");
    let signed = chain.sign(&plan, &seed()).unwrap();
    let decoded = chain.decode(&signed).unwrap();
    check_signed(planned, &decoded).unwrap();
    assert_eq!(decoded.sequence, chain.decode(&original).unwrap().sequence);
    assert_ne!(signed.id, original.id);
}

#[tokio::test]
async fn a_cancel_sends_the_same_coins_back_to_the_wallet() {
    let (chain, original) = sent("0.00025", false).await;
    let (plan, fee) = chain.replace(&original, 0, Replace::Cancel).await.unwrap();
    assert_eq!(fee.expected().to_string(), "0.00000392");
    let planned = chain.planned(&plan);
    assert_eq!(planned.amount.to_string(), "0.00029608");
    assert_ne!(planned.to, address(FRIEND));
    let signed = chain.sign(&plan, &seed()).unwrap();
    let decoded = chain.decode(&signed).unwrap();
    check_signed(planned, &decoded).unwrap();
    assert_eq!(decoded.sequence, chain.decode(&original).unwrap().sequence);
}

#[tokio::test]
async fn speeding_up_a_max_send_lowers_the_amount_by_the_extra_fee() {
    let (chain, original) = sent("0", true).await;
    let (plan, fee) = chain.replace(&original, 0, Replace::SpeedUp).await.unwrap();
    assert_eq!(fee.expected().to_string(), "0.00000534");
    assert_eq!(chain.planned(&plan).amount.to_string(), "0.00049466");
    let signed = chain.sign(&plan, &seed()).unwrap();
    check_signed(chain.planned(&plan), &chain.decode(&signed).unwrap()).unwrap();
}

#[tokio::test]
async fn a_legacy_send_is_sped_up_with_freshly_signed_legacy_inputs() {
    let (chain, original) = sent_as(
        BitcoinType::Legacy,
        [
            "1LqBGSKuX5yYUonjxT5qGfpUsXKYYWeabA",
            "1Ak8PffB2meyfYnbXZR9EGfLfFZVpzJvQP",
            "1J3J6EvPrv8q6AC3VCjWV45Uf3nssNMRtH",
        ],
        "0.00025",
        false,
    )
    .await;
    let (plan, fee) = chain.replace(&original, 0, Replace::SpeedUp).await.unwrap();
    assert_eq!(fee.replaced().unwrap().fee.to_string(), "0.00000448");
    assert_eq!(fee.expected().to_string(), "0.00000674");
    let signed = chain.sign(&plan, &seed()).unwrap();
    let decoded = chain.decode(&signed).unwrap();
    check_signed(chain.planned(&plan), &decoded).unwrap();
    assert_eq!(decoded.sequence, chain.decode(&original).unwrap().sequence);
}
