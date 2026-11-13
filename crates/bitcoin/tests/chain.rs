#![allow(clippy::unwrap_used)]

mod common;

use std::sync::{Arc, Mutex};

use bahaa_bitcoin::BitcoinChain;
use bahaa_chain::{Chain, ChainError, RateUnit, Transfer, TxStatus, Wait, check_signed};
use bahaa_crypto::{Seed, secp256k1_seed};
use bahaa_testkit::electrum::{Ledger, Log, called};
use bahaa_types::{Amount, BitcoinType, hex};
use common::{FIRST, FIRST_CHANGE, FRIEND, GENESIS, SECOND, address, bitcoin_on, native, seed};

async fn started(ledger: Ledger) -> (BitcoinChain, Log, Arc<Mutex<Ledger>>) {
    let log = Log::default();
    let ledger = Arc::new(Mutex::new(ledger));
    let url = bahaa_testkit::electrum::serve(Arc::clone(&log), Arc::clone(&ledger)).await;
    (BitcoinChain::new(bitcoin_on(&url)).unwrap(), log, ledger)
}

fn transfer(amount: &str, max: bool) -> Transfer {
    Transfer {
        account: 0,
        from: address(FIRST),
        to: address(FRIEND),
        asset: native(),
        amount: Amount::parse(amount, 8).unwrap(),
        max,
        memo: None,
    }
}

fn funded() -> Ledger {
    let mut ledger = Ledger::new(GENESIS);
    ledger.fund(&address(SECOND), 30_000, 1);
    ledger.fund(&address(FIRST_CHANGE), 20_000, 2);
    ledger
}

#[tokio::test]
async fn a_phrase_account_is_found_on_both_chains_up_to_the_gap() {
    let (chain, log, _) = started(funded()).await;
    assert_eq!(chain.derive(&seed(), 0).unwrap(), address(FIRST));
    let held = chain.balances(&address(FIRST), &[native()]).await.unwrap();
    assert_eq!(held[0].amount.to_string(), "0.0005");
    let histories = called(&log, "blockchain.scripthash.get_history");
    assert_eq!(histories.len(), 40 + 2 + 1);
    assert_eq!(called(&log, "blockchain.scripthash.listunspent").len(), 2);
    assert_eq!(called(&log, "server.version").len(), 1);
}

#[tokio::test]
async fn a_single_key_or_watched_address_is_one_address() {
    let mut ledger = Ledger::new(GENESIS);
    ledger.fund(&address(FRIEND), 7_000, 3);
    let (chain, log, _) = started(ledger).await;
    let held = chain.balances(&address(FRIEND), &[native()]).await.unwrap();
    assert_eq!(held[0].amount.to_string(), "0.00007");
    assert_eq!(
        called(&log, "blockchain.scripthash.get_history"),
        Vec::<serde_json::Value>::new()
    );
    let key: Seed = secp256k1_seed(&[0x11; 32]).unwrap();
    let single = chain.derive(&key, 4).unwrap();
    assert_eq!(single, chain.derive(&key, 0).unwrap());
    assert!(single.to_string().starts_with("bc1q"));
}

#[tokio::test]
async fn a_send_spends_the_largest_confirmed_coin_and_reads_back_from_its_bytes() {
    let (chain, log, ledger) = started(funded()).await;
    chain.derive(&seed(), 0).unwrap();
    let (plan, fee) = chain.prepare(&transfer("0.00025", false)).await.unwrap();
    assert_eq!(fee.expected().to_string(), "0.00000282");
    assert_eq!(fee.max(), fee.expected());
    let pace = fee.pace();
    assert_eq!(pace.rate.unwrap().amount.to_string(), "2");
    assert_eq!(pace.rate.unwrap().unit, RateUnit::SatPerVbyte);
    assert_eq!(pace.wait, Some(Wait::About(1_200)));
    let signed = chain.sign(&plan, &seed()).unwrap();
    let decoded = chain.decode(&signed).unwrap();
    check_signed(chain.planned(&plan), &decoded).unwrap();
    assert_eq!(decoded.from, address(FIRST));
    let id = chain.broadcast(&signed).await.unwrap();
    assert_eq!(id, signed.id);
    assert_eq!(
        ledger.lock().unwrap().broadcast,
        [hex::encode(&signed.bytes)]
    );
    assert!(called(&log, "blockchain.estimatefee").contains(&serde_json::json!([2])));
    let max = chain.prepare(&transfer("0", true)).await.unwrap();
    assert_eq!(chain.planned(&max.0).amount.to_string(), "0.00049644");
}

#[tokio::test]
async fn a_confirmed_send_reports_its_block_and_the_fee_it_paid() {
    let (chain, _, ledger) = started(funded()).await;
    chain.derive(&seed(), 0).unwrap();
    let (plan, _) = chain.prepare(&transfer("0.00025", false)).await.unwrap();
    let signed = chain.sign(&plan, &seed()).unwrap();
    assert_eq!(
        chain
            .status(&signed.id, &address(FIRST), None)
            .await
            .unwrap(),
        TxStatus::NotFound
    );
    chain.broadcast(&signed).await.unwrap();
    assert_eq!(
        chain
            .status(&signed.id, &address(FIRST), None)
            .await
            .unwrap(),
        TxStatus::Pending
    );
    ledger
        .lock()
        .unwrap()
        .confirm(signed.id.as_str(), &address(SECOND), 900_001);
    let TxStatus::Confirmed { block, fee, .. } = chain
        .status(&signed.id, &address(FIRST), None)
        .await
        .unwrap()
    else {
        panic!("not confirmed");
    };
    assert_eq!(block, Some(900_001));
    assert_eq!(fee.unwrap().to_string(), "0.00000282");
}

#[tokio::test]
async fn sends_that_cannot_work_are_refused_before_signing() {
    let (chain, _, ledger) = started(funded()).await;
    chain.derive(&seed(), 0).unwrap();
    let short = chain.prepare(&transfer("0.0005", false)).await.unwrap_err();
    assert!(
        matches!(&short, ChainError::Insufficient { needed, .. } if needed.to_string() == "0.00050356"),
        "{short}"
    );
    let dust = chain
        .prepare(&transfer("0.00000200", false))
        .await
        .unwrap_err();
    assert!(matches!(dust, ChainError::Invalid(_)), "{dust}");
    let memo = Transfer {
        memo: Some("hi".to_owned()),
        ..transfer("0.0001", false)
    };
    assert!(chain.prepare(&memo).await.is_err());
    let testnet = Transfer {
        to: address("tb1qw508d6qejxtdg4y5r3zarvary0c5xw7kxpjzsx"),
        ..transfer("0.0001", false)
    };
    assert!(chain.prepare(&testnet).await.is_err());
    let fresh = started(funded()).await.0;
    fresh.derive(&seed(), 0).unwrap();
    let change = Transfer {
        to: address(FIRST_CHANGE),
        ..transfer("0.0001", false)
    };
    assert!(fresh.prepare(&change).await.is_err());
    ledger.lock().unwrap().refusal = Some("min relay fee not met".to_owned());
    let (plan, _) = chain.prepare(&transfer("0.0001", false)).await.unwrap();
    let signed = chain.sign(&plan, &seed()).unwrap();
    assert_eq!(
        chain.broadcast(&signed).await.unwrap_err(),
        ChainError::Rejected("min relay fee not met".to_owned())
    );
}

#[tokio::test]
async fn a_server_on_another_network_is_never_used() {
    let mut ledger = funded();
    ledger.genesis = "00".repeat(32);
    let (chain, _, _) = started(ledger).await;
    let failed = chain
        .balances(&address(FRIEND), &[native()])
        .await
        .unwrap_err();
    assert!(failed.to_string().contains("another network"), "{failed}");
}

#[tokio::test]
async fn another_seed_cannot_sign_for_this_account() {
    let (chain, _, _) = started(funded()).await;
    chain.derive(&seed(), 0).unwrap();
    let (plan, _) = chain.prepare(&transfer("0.0001", false)).await.unwrap();
    let other = bahaa_crypto::Mnemonic::parse(
        "shoot island position soft burden budget tooth cruel issue economy destroy above",
    )
    .unwrap()
    .to_seed("");
    assert!(chain.sign(&plan, &other).is_err());
}

#[tokio::test]
async fn a_server_cannot_invent_coins() {
    let mut inflated = funded();
    for coins in inflated.unspent.values_mut() {
        coins[0]["value"] = 90_000.into();
    }
    let (chain, _, _) = started(inflated).await;
    chain.derive(&seed(), 0).unwrap();
    let failed = chain
        .balances(&address(FIRST), &[native()])
        .await
        .unwrap_err();
    assert!(failed.to_string().contains("could not prove"), "{failed}");
    let mut forged = funded();
    forged.forge_proofs = true;
    let (chain, _, _) = started(forged).await;
    chain.derive(&seed(), 0).unwrap();
    let failed = chain
        .balances(&address(FIRST), &[native()])
        .await
        .unwrap_err();
    assert!(failed.to_string().contains("could not prove"), "{failed}");
}

#[tokio::test]
async fn a_send_from_several_coins_signs_every_input_with_its_own_key() {
    let mut ledger = funded();
    ledger.fund(&address(SECOND), 7_000, 3);
    let (chain, _, _) = started(ledger).await;
    chain.derive(&seed(), 0).unwrap();
    let (plan, _) = chain.prepare(&transfer("0", true)).await.unwrap();
    let signed = chain.sign(&plan, &seed()).unwrap();
    let decoded = chain.decode(&signed).unwrap();
    check_signed(chain.planned(&plan), &decoded).unwrap();
    assert_eq!(decoded.amount.to_string(), "0.00056506");
}

#[tokio::test]
async fn a_taproot_account_follows_bip86_and_signs_with_schnorr() {
    const TAPROOT_FIRST: &str = "bc1p5cyxnuxmeuwuvkwfem96lqzszd02n6xdcjrs20cac6yqjjwudpxqkedrcr";
    const TAPROOT_SECOND: &str = "bc1p4qhjn9zdvkux4e44uhx8tc55attvtyu358kutcqkudyccelu0was9fqzwh";
    let mut ledger = Ledger::new(GENESIS);
    ledger.fund(&address(TAPROOT_SECOND), 30_000, 1);
    let (chain, _, ledger) = started(ledger).await;
    let first = chain.derive_as(&seed(), 0, BitcoinType::Taproot).unwrap();
    assert_eq!(first, address(TAPROOT_FIRST));
    let held = chain.balances(&first, &[native()]).await.unwrap();
    assert_eq!(held[0].amount.to_string(), "0.0003");
    let transfer = Transfer {
        from: first,
        ..transfer("0.00025", false)
    };
    let (plan, fee) = chain.prepare(&transfer).await.unwrap();
    assert_eq!(fee.expected().to_string(), "0.00000284");
    let signed = chain.sign(&plan, &seed()).unwrap();
    let decoded = chain.decode(&signed).unwrap();
    check_signed(chain.planned(&plan), &decoded).unwrap();
    chain.broadcast(&signed).await.unwrap();
    assert_eq!(ledger.lock().unwrap().broadcast.len(), 1);
    let mut forged = signed.clone();
    let last = forged.bytes.len() - 6;
    forged.bytes[last] ^= 1;
    assert!(chain.decode(&forged).is_err());
}

async fn sent_from(kind: BitcoinType, first: &str, second: &str, change: &str) -> (u64, String) {
    let mut ledger = Ledger::new(GENESIS);
    ledger.fund(&address(second), 30_000, 1);
    ledger.fund(&address(change), 20_000, 2);
    let (chain, _, ledger) = started(ledger).await;
    let from = chain.derive_as(&seed(), 0, kind).unwrap();
    assert_eq!(from, address(first));
    let held = chain.balances(&from, &[native()]).await.unwrap();
    assert_eq!(held[0].amount.to_string(), "0.0005");
    let transfer = Transfer {
        from,
        ..transfer("0.00025", false)
    };
    let (plan, fee) = chain.prepare(&transfer).await.unwrap();
    let signed = chain.sign(&plan, &seed()).unwrap();
    let decoded = chain.decode(&signed).unwrap();
    check_signed(chain.planned(&plan), &decoded).unwrap();
    assert_eq!(decoded.from, from);
    chain.broadcast(&signed).await.unwrap();
    assert_eq!(ledger.lock().unwrap().broadcast.len(), 1);
    let mut forged = signed.clone();
    let at = forged
        .bytes
        .windows(3)
        .position(|window| window[0] == 0x02 && window[1] == 0x20)
        .unwrap()
        + 10;
    forged.bytes[at] ^= 1;
    assert!(chain.decode(&forged).is_err());
    let units = u64::try_from(fee.expected().units()).unwrap();
    (units, hex::encode(&signed.bytes))
}

#[tokio::test]
async fn a_legacy_account_follows_bip44_and_signs_its_inputs_in_place() {
    let (fee, raw) = sent_from(
        BitcoinType::Legacy,
        "1LqBGSKuX5yYUonjxT5qGfpUsXKYYWeabA",
        "1Ak8PffB2meyfYnbXZR9EGfLfFZVpzJvQP",
        "1J3J6EvPrv8q6AC3VCjWV45Uf3nssNMRtH",
    )
    .await;
    assert_eq!(fee, 448);
    assert!(raw.starts_with("0200000001"), "{raw}");
}

#[tokio::test]
async fn a_nested_segwit_account_follows_bip49_and_wraps_its_witness() {
    let (fee, raw) = sent_from(
        BitcoinType::NestedSegwit,
        "37VucYSaXLCAsxYyAPfbSi9eh4iEcbShgf",
        "3LtMnn87fqUeHBUG414p9CWwnoV6E2pNKS",
        "34K56kSjgUCUSD8GTtuF7c9Zzwokbs6uZ7",
    )
    .await;
    assert_eq!(fee, 330);
    assert!(raw.starts_with("020000000001"), "{raw}");
}

#[tokio::test]
async fn an_uncompressed_key_has_only_its_old_legacy_address() {
    let mut secret = [0u8; 32];
    secret[31] = 1;
    let key = secp256k1_seed(&secret).unwrap();
    let mut ledger = Ledger::new(GENESIS);
    let old = address("1EHNa6Q4Jz2uvNExL497mE43ikXhwF6kZm");
    ledger.fund(&old, 30_000, 1);
    let (chain, _, _) = started(ledger).await;
    assert_eq!(
        chain
            .derive_as(&key, 0, BitcoinType::LegacyUncompressed)
            .unwrap(),
        old
    );
    assert_eq!(
        chain.derive_as(&key, 0, BitcoinType::Legacy).unwrap(),
        address("1BgGZ9tcN4rm9KBzDn7KprQz87SZ26SAMH")
    );
    assert!(
        chain
            .derive_as(&seed(), 0, BitcoinType::LegacyUncompressed)
            .is_err()
    );
    chain
        .derive_as(&key, 0, BitcoinType::LegacyUncompressed)
        .unwrap();
    let transfer = Transfer {
        from: old,
        ..transfer("0.00025", false)
    };
    let (plan, fee) = chain.prepare(&transfer).await.unwrap();
    assert_eq!(fee.expected().to_string(), "0.00000512");
    let signed = chain.sign(&plan, &key).unwrap();
    check_signed(chain.planned(&plan), &chain.decode(&signed).unwrap()).unwrap();
    assert!(hex::encode(&signed.bytes).contains("410479be667e"));
}

#[tokio::test]
async fn an_unconfirmed_legacy_coin_must_match_its_funding_transaction() {
    let mut ledger = Ledger::new(GENESIS);
    let first = address("1LqBGSKuX5yYUonjxT5qGfpUsXKYYWeabA");
    ledger.fund_at(&first, 30_000, 1, 0);
    let (chain, log, _) = started(ledger.clone()).await;
    chain.derive_as(&seed(), 0, BitcoinType::Legacy).unwrap();
    let held = chain.balances(&first, &[native()]).await.unwrap();
    assert_eq!(held[0].amount.to_string(), "0.0003");
    assert_eq!(called(&log, "blockchain.transaction.get").len(), 1);
    for coins in ledger.unspent.values_mut() {
        coins[0]["value"] = 90_000.into();
    }
    let (chain, _, _) = started(ledger).await;
    chain.derive_as(&seed(), 0, BitcoinType::Legacy).unwrap();
    let failed = chain.balances(&first, &[native()]).await.unwrap_err();
    assert!(failed.to_string().contains("could not prove"), "{failed}");
}

#[tokio::test]
async fn a_server_that_cannot_estimate_slow_targets_still_quotes() {
    let mut ledger = funded();
    ledger.unestimated = vec![72, 144];
    let (chain, _, _) = started(ledger).await;
    chain.derive(&seed(), 0).unwrap();
    let (_, fee) = chain.prepare(&transfer("0.00025", false)).await.unwrap();
    assert_eq!(fee.expected().to_string(), "0.00000282");
    assert_eq!(fee.pace().wait, Some(Wait::About(1_200)));
    let mut ledger = funded();
    ledger.unestimated = vec![2];
    let (chain, _, _) = started(ledger).await;
    chain.derive(&seed(), 0).unwrap();
    let failed = chain
        .prepare(&transfer("0.00025", false))
        .await
        .unwrap_err();
    assert!(failed.to_string().contains("cannot estimate"), "{failed}");
}
