#![allow(clippy::unwrap_used)]

mod common;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use bahaa_config::Registry;
use bahaa_crypto::SecretString;
use bahaa_engine::swap::{Method, Providers, SwapStatus};
use bahaa_engine::{
    Engine, Event, SendRequest, SentKind, SentStatus, SolanaPath, Spend, SwapRequest, WalletId,
    WalletKind,
};
use bahaa_types::{Address, Amount, AssetId, ChainFamily};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use common::{Scratch, config, result, settled, until};
use serde_json::{Value, json};

const PHRASE: &str =
    "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";
const OWNER: &str = "HAgk14JpMQLgt6rVgv7cBQFJWFto5Dqxi472uT3DKpqk";
const FRIEND: &str = "2bUBiBNZyD29gP1oV6de7nxowMLoDBtopMMTGgMvjG5m";
const SYSTEM: &str = "11111111111111111111111111111111";
const TOKEN: &str = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA";
const USDT: &str = "Es9vMFrzaCERmJfrF4H2FYD4KCoNkY11McCe8BenwNYB";

#[derive(Default)]
struct Ledger {
    sent: Vec<String>,
}

fn account(lamports: u64) -> Value {
    json!({"lamports": lamports, "owner": SYSTEM, "executable": false, "data": ["", "base64"], "space": 0})
}

fn mint(decimals: u8) -> Value {
    let mut data = vec![0u8; 82];
    data[44] = decimals;
    data[45] = 1;
    json!({"lamports": 1, "owner": TOKEN, "executable": false, "data": [STANDARD.encode(data), "base64"], "space": 82})
}

fn answer(ledger: &Mutex<Ledger>, call: &Value) -> Value {
    let params = &call["params"];
    let mut ledger = ledger.lock().unwrap();
    result(match call["method"].as_str().unwrap() {
        "getGenesisHash" => json!("5eykt4UsFv8P8NJdTREpY1vzqKqZKvdpKuc147dw2N9d"),
        "getMultipleAccounts" => {
            let listed: Vec<Value> = params[0]
                .as_array()
                .unwrap()
                .iter()
                .map(|key| match key.as_str().unwrap() {
                    OWNER => account(1_250_000_000),
                    FRIEND => account(1),
                    USDT | USDC => mint(6),
                    _ => Value::Null,
                })
                .collect();
            json!({"context": {"slot": 1}, "value": listed})
        }
        "getMinimumBalanceForRentExemption" => json!(650_240),
        "getLatestBlockhash" => {
            json!({"context": {"slot": 1}, "value": {"blockhash": "Aiyo4Fg2XByNaEhKv3LtX5h9Q8Y2kzhhCZhsnV5fBsrw", "lastValidBlockHeight": 9}})
        }
        "getRecentPrioritizationFees" => json!([{"slot": 1, "prioritizationFee": 1_000}]),
        "getSignaturesForAddress" if params[0] == FRIEND && params[1]["limit"] == 1 => json!([
            {"signature": "vGWaZAYtYPZpRsBbKL1LyTZS2Upj7muftZzAH4PJfWif3XAk8vFEiV9XVtDeVRWAURJVpFYBt1sbRXxrdBRMCAQ", "slot": 9, "blockTime": 1_793_637_863, "err": null}
        ]),
        "getSignaturesForAddress" => json!([]),
        "getAssetsByOwner" => json!({"total": 0, "limit": 1000, "page": 1, "items": []}),
        "simulateTransaction" if params[1]["innerInstructions"] == true => swap_simulation(),
        "simulateTransaction" => {
            json!({"context": {"slot": 1}, "value": {"err": null, "logs": [], "unitsConsumed": 450}})
        }
        "sendTransaction" => {
            let bytes = STANDARD.decode(params[0].as_str().unwrap()).unwrap();
            let id = bs58::encode(&bytes[1..65]).into_string();
            ledger.sent.push(params[0].as_str().unwrap().to_owned());
            json!(id)
        }
        "getSignatureStatuses" => {
            json!({"context": {"slot": 1}, "value": [{"slot": 77, "err": null, "confirmationStatus": "confirmed"}]})
        }
        "getTransaction" => {
            json!({"slot": 77, "meta": {"fee": 5_001, "err": null}, "transaction": [ledger.sent[0], "base64"]})
        }
        other => panic!("unexpected {other}"),
    })
}

async fn started() -> (Scratch, Engine, Arc<Mutex<Ledger>>) {
    let scratch = Scratch::new();
    let ledger = Arc::new(Mutex::new(Ledger::default()));
    let shared = Arc::clone(&ledger);
    let url = common::node(move |call| (answer(&shared, call), Duration::ZERO)).await;
    let registry = Registry::with_networks(vec![common::solana_network(url)]).unwrap();
    let engine = Engine::start(config(&scratch, registry, None)).unwrap();
    engine.create(SecretString::from("pw")).await.unwrap();
    (scratch, engine, ledger)
}

async fn solana_path(engine: &Engine, wallet: WalletId) -> Option<SolanaPath> {
    engine
        .wallets()
        .await
        .unwrap()
        .into_iter()
        .find(|info| info.id == wallet)
        .unwrap()
        .solana
}

#[tokio::test]
async fn a_recovery_phrase_holds_sol_and_sends_it_within_the_quote_life() {
    let (_scratch, engine, ledger) = started().await;
    let mut events = engine.events();
    let wallet = engine
        .import(SecretString::from(PHRASE), SecretString::from(""))
        .await
        .unwrap();
    let mut portfolio = engine.portfolio();
    let held = until(&mut portfolio, |portfolio| !portfolio.holdings.is_empty()).await;
    assert_eq!(held.holdings[0].address.to_string(), OWNER);
    assert_eq!(held.holdings[0].amount.to_string(), "1.25");
    let request = SendRequest {
        account: 0,
        wallet,
        asset: "solana".parse::<AssetId>().unwrap(),
        to: Address::parse(ChainFamily::Solana, FRIEND).unwrap(),
        amount: Spend::Exact(Amount::parse("0.1", 9).unwrap()),
        memo: Some("dinner".to_owned()),
    };
    let draft = engine.prepare_send(request).await.unwrap();
    assert_eq!(draft.valid_for, bahaa_solana::QUOTE_LIFE);
    assert_eq!(draft.fee.expected().to_string(), "0.000005001");
    let sent = engine
        .confirm_send(draft.id, SecretString::from("pw"))
        .await
        .unwrap();
    assert_eq!(sent.from.to_string(), OWNER);
    assert_eq!(sent.memo.as_deref(), Some("dinner"));
    assert_eq!(ledger.lock().unwrap().sent.len(), 1);
    let done = settled(&mut events, &sent.id).await;
    assert_eq!(done.status, SentStatus::Confirmed);
    assert_eq!(done.block, Some(77));
    assert_eq!(
        done.fee_paid,
        Some(Amount::parse("0.000005001", 9).unwrap())
    );
}

#[tokio::test]
async fn a_solana_key_and_a_watched_address_hold_only_their_account() {
    let (_scratch, engine, _) = started().await;
    let exported = bahaa_crypto::solana_keypair(&[0x9d; 32]);
    let key = engine
        .import_key(
            SecretString::from(exported.to_string()),
            ChainFamily::Solana,
        )
        .await
        .unwrap();
    let watched = engine
        .watch(Address::parse(ChainFamily::Solana, OWNER).unwrap(), true)
        .await
        .unwrap();
    let kinds: Vec<WalletKind> = engine
        .wallets()
        .await
        .unwrap()
        .iter()
        .map(|wallet| wallet.kind)
        .collect();
    assert_eq!(
        kinds,
        [
            WalletKind::Key(ChainFamily::Solana),
            WalletKind::Watch(ChainFamily::Solana)
        ]
    );
    let mut portfolio = engine.portfolio();
    let held = until(&mut portfolio, |portfolio| {
        portfolio
            .holdings
            .iter()
            .any(|holding| holding.wallet == watched)
    })
    .await;
    assert!(
        held.holdings
            .iter()
            .all(|holding| holding.asset.network().as_str() == "solana")
    );
    assert!(held.holdings.iter().any(|holding| holding.wallet == key));
}

#[tokio::test]
async fn a_phrase_used_on_the_short_path_is_found_there_and_remembered() {
    let (_scratch, engine, _) = started().await;
    let trust = "shoot island position soft burden budget tooth cruel issue economy destroy above";
    let wallet = engine
        .import(SecretString::from(trust), SecretString::from(""))
        .await
        .unwrap();
    let mut portfolio = engine.portfolio();
    until(&mut portfolio, |portfolio| {
        portfolio
            .holdings
            .iter()
            .any(|holding| holding.address.to_string() == FRIEND)
    })
    .await;
    assert_eq!(solana_path(&engine, wallet).await, Some(SolanaPath::Bip44));
    engine.lock().await.unwrap();
    engine.unlock(SecretString::from("pw")).await.unwrap();
    assert_eq!(solana_path(&engine, wallet).await, Some(SolanaPath::Bip44));
    engine
        .set_solana_path(wallet, SolanaPath::Bip44Change)
        .await
        .unwrap();
    assert_eq!(
        solana_path(&engine, wallet).await,
        Some(SolanaPath::Bip44Change)
    );
}

const JUPITER: &str = "JUP6LkbZbjS1jKKwapdHNy74zcZ3tLUZoi5QNyVTaV4";
const USDC: &str = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v";
const SOLD: u64 = 100_000_000;
const QUOTED: u64 = 9_331_279;
const RENT: u64 = 2_039_280;

fn key(text: &str) -> [u8; 32] {
    bs58::decode(text).into_vec().unwrap().try_into().unwrap()
}

fn swap_transaction() -> Vec<u8> {
    let mut data = vec![0xe5, 0x17, 0xcb, 0x97, 0x7a, 0xe3, 0xad, 0x2a, 0];
    data.extend_from_slice(&SOLD.to_le_bytes());
    data.extend_from_slice(&QUOTED.to_le_bytes());
    data.extend_from_slice(&50u16.to_le_bytes());
    data.push(0);
    let mut message = vec![0x80, 1, 0, 1, 3];
    for account in [key(OWNER), [5; 32], key(JUPITER)] {
        message.extend_from_slice(&account);
    }
    message.extend_from_slice(&[7; 32]);
    message.extend_from_slice(&[1, 2, 2, 0, 1]);
    message.push(u8::try_from(data.len()).unwrap());
    message.extend_from_slice(&data);
    message.push(0);
    let mut transaction = vec![1];
    transaction.extend_from_slice(&[0; 64]);
    transaction.extend_from_slice(&message);
    transaction
}

fn swap_simulation() -> Value {
    let start = 1_250_000_000u64;
    json!({"context": {"slot": 1}, "value": {
        "err": null,
        "fee": 5_000,
        "unitsConsumed": 50_000,
        "loadedAddresses": {"writable": [], "readonly": []},
        "preBalances": [start, 0, 1],
        "postBalances": [start - SOLD - 5_000 - RENT, RENT, 1],
        "preTokenBalances": [],
        "postTokenBalances": [{
            "accountIndex": 1,
            "mint": USDC,
            "owner": OWNER,
            "uiTokenAmount": {"amount": QUOTED.to_string(), "decimals": 6},
        }],
        "innerInstructions": [],
        "accounts": [account(start - SOLD - 5_000 - RENT)],
    }})
}

async fn jupiter() -> url::Url {
    bahaa_testkit::serve(|request| {
        if request.path().starts_with("/quote") {
            return bahaa_testkit::Reply::json(
                200,
                &json!({
                    "inputMint": "So11111111111111111111111111111111111111112",
                    "outputMint": USDC,
                    "inAmount": SOLD.to_string(),
                    "outAmount": QUOTED.to_string(),
                    "otherAmountThreshold": "9284623",
                    "swapMode": "ExactIn",
                    "platformFee": null,
                }),
            );
        }
        bahaa_testkit::Reply::json(
            200,
            &json!({"swapTransaction": STANDARD.encode(swap_transaction())}),
        )
    })
    .await
}

#[tokio::test]
async fn a_jupiter_swap_is_checked_by_simulation_signed_sent_and_settled() {
    let scratch = Scratch::new();
    let ledger = Arc::new(Mutex::new(Ledger::default()));
    let shared = Arc::clone(&ledger);
    let url = common::node(move |call| (answer(&shared, call), Duration::ZERO)).await;
    let registry = Registry::with_networks(vec![common::solana_network(url)]).unwrap();
    let dead = url::Url::parse("http://127.0.0.1:9/").unwrap();
    let mut providers = Providers::builtin().unwrap();
    providers.oneclick.url = dead.clone();
    providers.relay.url = dead;
    providers.jupiter.url = jupiter().await;
    let mut config = config(&scratch, registry, None);
    config.swaps = Some(providers);
    let engine = Engine::start(config).unwrap();
    engine.create(SecretString::from("pw")).await.unwrap();
    let wallet = engine
        .import(SecretString::from(PHRASE), SecretString::from(""))
        .await
        .unwrap();
    let mut events = engine.events();
    let draft = engine
        .prepare_swap(SwapRequest {
            wallet,
            account: 0,
            sell: "solana".parse().unwrap(),
            buy: format!("solana/{USDC}").parse().unwrap(),
            amount: Spend::Exact(Amount::parse("0.1", 9).unwrap()),
        })
        .await
        .unwrap();
    let index = draft
        .routes
        .iter()
        .position(|ranked| ranked.route.provider == "Jupiter")
        .unwrap();
    let route = &draft.routes[index].route;
    assert_eq!(route.method, Method::Aggregator);
    assert_eq!(route.minimum.to_string(), "9.284622");
    let review = engine.choose_swap(draft.id, index).await.unwrap();
    assert!(review.checked);
    assert_eq!(review.draft.fee.expected().to_string(), "0.00204428");
    let tx = engine
        .confirm_send(review.draft.id, SecretString::from("pw"))
        .await
        .unwrap();
    assert_eq!(tx.kind, SentKind::Swap);
    assert_eq!(ledger.lock().unwrap().sent.len(), 1);
    let done = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Ok(Event::SwapUpdated(swap)) = events.recv().await
                && swap.status == SwapStatus::Done
            {
                return swap;
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(done.provider, "jupiter");
    assert_eq!(done.reference, tx.id);
}
