#![allow(clippy::unwrap_used)]

mod common;

use std::sync::{Arc, Mutex};

use bahaa_config::Registry;
use bahaa_crypto::{ExposeSecret, SecretString};
use bahaa_engine::{Engine, Event, Replace, SendRequest, SentStatus, Spend, WalletKind};
use bahaa_testkit::electrum::{Ledger, Log, sender};
use bahaa_types::{Address, Amount, AssetId, BitcoinType, ChainFamily};
use common::{BITCOIN_GENESIS, Scratch, bitcoin_network, config, settled, until};

const PHRASE: &str =
    "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";
const FIRST: &str = "bc1qcr8te4kr609gcawutmrza0j4xv80jy8z306fyu";
const SECOND: &str = "bc1qnjg0jd8228aq7egyzacy8cys3knf9xvrerkf9g";
const FRIEND: &str = "bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t4";

fn address(text: &str) -> Address {
    Address::parse(ChainFamily::Bitcoin, text).unwrap()
}

async fn started() -> (Scratch, Engine, Arc<Mutex<Ledger>>) {
    let mut ledger = Ledger::new(BITCOIN_GENESIS);
    ledger.fund(&address(SECOND), 50_000, 1);
    let ledger = Arc::new(Mutex::new(ledger));
    let url = bahaa_testkit::electrum::serve(Log::default(), Arc::clone(&ledger)).await;
    let scratch = Scratch::new();
    let registry = Registry::with_networks(vec![bitcoin_network(url)]).unwrap();
    let engine = Engine::start(config(&scratch, registry, None)).unwrap();
    engine.create(SecretString::from("pw")).await.unwrap();
    (scratch, engine, ledger)
}

#[tokio::test]
async fn a_phrase_holds_bitcoin_across_its_addresses_and_sends_it() {
    let (_scratch, engine, ledger) = started().await;
    let mut events = engine.events();
    let wallet = engine
        .import(SecretString::from(PHRASE), SecretString::from(""))
        .await
        .unwrap();
    let mut portfolio = engine.portfolio();
    let held = until(&mut portfolio, |portfolio| !portfolio.holdings.is_empty()).await;
    assert_eq!(held.holdings[0].address.to_string(), FIRST);
    assert_eq!(held.holdings[0].amount.to_string(), "0.0005");
    let arrived = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            if let Ok(Event::Received(arrivals)) = events.recv().await {
                return arrivals;
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(arrived[0].received.from, sender(1));
    assert_eq!(arrived[0].received.to, address(FIRST));
    assert_eq!(arrived[0].received.amount.to_string(), "0.0005");
    let request = SendRequest {
        wallet,
        account: 0,
        asset: "bitcoin".parse::<AssetId>().unwrap(),
        to: address(FRIEND),
        amount: Spend::Exact(Amount::parse("0.0002", 8).unwrap()),
        memo: None,
    };
    let draft = engine.prepare_send(request).await.unwrap();
    assert_eq!(draft.fee.expected().to_string(), "0.00000282");
    let sent = engine
        .confirm_send(draft.id, SecretString::from("pw"))
        .await
        .unwrap();
    assert_eq!(sent.from.to_string(), FIRST);
    assert_eq!(ledger.lock().unwrap().broadcast.len(), 1);
    ledger
        .lock()
        .unwrap()
        .confirm(&sent.id, &address(SECOND), 900_001);
    let done = settled(&mut events, &sent.id).await;
    assert_eq!(done.status, SentStatus::Confirmed);
    assert_eq!(done.block, Some(900_001));
    assert_eq!(done.fee_paid, Some(Amount::parse("0.00000282", 8).unwrap()));
    engine.lock().await.unwrap();
    engine.unlock(SecretString::from("pw")).await.unwrap();
    let mut portfolio = engine.portfolio();
    let again = until(&mut portfolio, |portfolio| !portfolio.holdings.is_empty()).await;
    assert_eq!(again.holdings[0].amount.to_string(), "0.0005");
}

#[tokio::test]
async fn a_bitcoin_key_and_a_watched_address_hold_one_address_each() {
    let (_scratch, engine, _) = started().await;
    let wif = bahaa_crypto::bitcoin_wif(&[0x9d; 32], true);
    let key = engine
        .import_key(SecretString::from(wif.to_string()), ChainFamily::Bitcoin)
        .await
        .unwrap();
    let watched = engine.watch(address(SECOND), true).await.unwrap();
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
            WalletKind::Key(ChainFamily::Bitcoin),
            WalletKind::Watch(ChainFamily::Bitcoin)
        ]
    );
    let mut portfolio = engine.portfolio();
    let held = until(&mut portfolio, |portfolio| {
        let has = |wallet| {
            portfolio
                .holdings
                .iter()
                .any(|holding| holding.wallet == wallet)
        };
        has(watched) && has(key)
    })
    .await;
    let watched_held = held
        .holdings
        .iter()
        .find(|holding| holding.wallet == watched)
        .unwrap();
    assert_eq!(watched_held.amount.to_string(), "0.0005");
    assert_eq!(
        engine
            .reveal(key, SecretString::from("pw"))
            .await
            .unwrap()
            .expose_secret(),
        wif.as_str()
    );
}

async fn detected(funded: &str, secret: &str, key: Option<ChainFamily>) -> BitcoinType {
    let mut ledger = Ledger::new(BITCOIN_GENESIS);
    ledger.fund(&address(funded), 40_000, 2);
    let ledger = Arc::new(Mutex::new(ledger));
    let url = bahaa_testkit::electrum::serve(Log::default(), Arc::clone(&ledger)).await;
    let scratch = Scratch::new();
    let registry = Registry::with_networks(vec![bitcoin_network(url)]).unwrap();
    let engine = Engine::start(config(&scratch, registry, None)).unwrap();
    engine.create(SecretString::from("pw")).await.unwrap();
    let wallet = match key {
        Some(family) => engine.import_key(SecretString::from(secret), family).await,
        None => {
            engine
                .import(SecretString::from(secret), SecretString::from(""))
                .await
        }
    }
    .unwrap();
    let mut portfolio = engine.portfolio();
    let held = until(&mut portfolio, |portfolio| {
        portfolio
            .holdings
            .iter()
            .any(|holding| holding.address.to_string() == funded)
    })
    .await;
    assert_eq!(held.holdings[0].amount.to_string(), "0.0004");
    let kind = |wallets: Vec<bahaa_engine::WalletInfo>| {
        wallets
            .into_iter()
            .find(|info| info.id == wallet)
            .unwrap()
            .bitcoin
            .unwrap()
    };
    let found = kind(engine.wallets().await.unwrap());
    engine.lock().await.unwrap();
    engine.unlock(SecretString::from("pw")).await.unwrap();
    assert_eq!(kind(engine.wallets().await.unwrap()), found);
    found
}

#[tokio::test]
async fn a_phrase_is_found_on_the_address_type_it_was_used_with_and_remembered() {
    for (funded, kind) in [
        (
            "bc1p5cyxnuxmeuwuvkwfem96lqzszd02n6xdcjrs20cac6yqjjwudpxqkedrcr",
            BitcoinType::Taproot,
        ),
        ("1LqBGSKuX5yYUonjxT5qGfpUsXKYYWeabA", BitcoinType::Legacy),
        (
            "37VucYSaXLCAsxYyAPfbSi9eh4iEcbShgf",
            BitcoinType::NestedSegwit,
        ),
    ] {
        assert_eq!(detected(funded, PHRASE, None).await, kind, "{funded}");
    }
}

#[tokio::test]
async fn old_keys_are_found_on_their_uncompressed_address() {
    const FIRST_KEY: &str = "0000000000000000000000000000000000000000000000000000000000000001";
    const UNCOMPRESSED: &str = "1EHNa6Q4Jz2uvNExL497mE43ikXhwF6kZm";
    assert_eq!(
        detected(UNCOMPRESSED, FIRST_KEY, Some(ChainFamily::Bitcoin)).await,
        BitcoinType::LegacyUncompressed
    );
    assert_eq!(
        detected(
            "1BgGZ9tcN4rm9KBzDn7KprQz87SZ26SAMH",
            FIRST_KEY,
            Some(ChainFamily::Bitcoin)
        )
        .await,
        BitcoinType::Legacy
    );
    let wif = bahaa_crypto::bitcoin_wif(&[0x9d; 32], false);
    assert!(wif.starts_with('5'));
    let (_scratch, engine, _) = started().await;
    let key = engine
        .import_key(SecretString::from(wif.to_string()), ChainFamily::Bitcoin)
        .await
        .unwrap();
    let info = |wallets: Vec<bahaa_engine::WalletInfo>| {
        wallets.into_iter().find(|info| info.id == key).unwrap()
    };
    assert_eq!(
        info(engine.wallets().await.unwrap()).bitcoin,
        Some(BitcoinType::LegacyUncompressed)
    );
    assert_eq!(
        engine
            .reveal(key, SecretString::from("pw"))
            .await
            .unwrap()
            .expose_secret(),
        wif.as_str()
    );
    let phrase = engine
        .import(SecretString::from(PHRASE), SecretString::from(""))
        .await
        .unwrap();
    assert!(
        engine
            .set_bitcoin_type(phrase, BitcoinType::LegacyUncompressed)
            .await
            .is_err()
    );
    engine
        .set_bitcoin_type(key, BitcoinType::Legacy)
        .await
        .unwrap();
    assert_eq!(
        engine
            .reveal(key, SecretString::from("pw"))
            .await
            .unwrap()
            .expose_secret()
            .chars()
            .next(),
        Some('L')
    );
}

#[tokio::test]
async fn a_speed_up_replaces_the_pending_send_and_retires_it_once_confirmed() {
    let (_scratch, engine, ledger) = started().await;
    let mut events = engine.events();
    let wallet = engine
        .import(SecretString::from(PHRASE), SecretString::from(""))
        .await
        .unwrap();
    let mut portfolio = engine.portfolio();
    until(&mut portfolio, |portfolio| !portfolio.holdings.is_empty()).await;
    let request = SendRequest {
        wallet,
        account: 0,
        asset: "bitcoin".parse::<AssetId>().unwrap(),
        to: address(FRIEND),
        amount: Spend::Exact(Amount::parse("0.0002", 8).unwrap()),
        memo: None,
    };
    let draft = engine.prepare_send(request).await.unwrap();
    let first = engine
        .confirm_send(draft.id, SecretString::from("pw"))
        .await
        .unwrap();
    let faster = engine
        .replace(
            "bitcoin".parse().unwrap(),
            first.id.clone(),
            Replace::SpeedUp,
        )
        .await
        .unwrap();
    assert_eq!(faster.replaces.as_deref(), Some(first.id.as_str()));
    assert_eq!(faster.fee.expected().to_string(), "0.00000423");
    let second = engine
        .confirm_send(faster.id, SecretString::from("pw"))
        .await
        .unwrap();
    ledger
        .lock()
        .unwrap()
        .confirm(&second.id, &address(SECOND), 900_001);
    let done = settled(&mut events, &second.id).await;
    assert_eq!(done.status, SentStatus::Confirmed);
    let history = engine.history().await.unwrap();
    let retired = history.iter().find(|tx| tx.id == first.id).unwrap();
    assert_eq!(retired.status, SentStatus::Dropped);
}

#[tokio::test]
async fn the_receive_address_moves_past_used_ones_and_other_networks_keep_theirs() {
    let (_scratch, engine, ledger) = started().await;
    engine
        .import(SecretString::from(PHRASE), SecretString::from(""))
        .await
        .unwrap();
    let mut portfolio = engine.portfolio();
    until(&mut portfolio, |portfolio| !portfolio.holdings.is_empty()).await;
    let bitcoin: bahaa_types::NetworkId = "bitcoin".parse().unwrap();
    let next = engine
        .receive_address(bitcoin.clone(), address(FIRST))
        .await
        .unwrap();
    assert_ne!(next, address(FIRST));
    assert_ne!(next, address(SECOND));
    ledger.lock().unwrap().fund(&next, 1_000, 9);
    let after = engine
        .receive_address(bitcoin.clone(), address(FIRST))
        .await
        .unwrap();
    assert_ne!(after, next);
    assert!(
        engine
            .receive_address(bitcoin, address(FRIEND))
            .await
            .is_err()
    );
}

const VAULT: &str = "bc1pske0hr4ne5xajxdd6gz25m6sdsy6xva0aly9phgl0xy0frj9rl0qc94fxv";
const HARDHAT_0: &str = "0x9858effd232b4033e47d90003d41ec34ecaeda94";

fn chainflip_side(
    state: Arc<Mutex<&'static str>>,
) -> impl Fn(bahaa_testkit::Request) -> bahaa_testkit::Reply + Send + Sync + 'static {
    move |request| {
        let path = request.path().to_owned();
        let reply = if path.starts_with("/v2/quote") {
            serde_json::json!([{
                "type": "REGULAR",
                "srcAsset": {"chain": "Bitcoin", "asset": "BTC"},
                "destAsset": {"chain": "Ethereum", "asset": "USDC"},
                "depositAmount": "20000",
                "egressAmount": "15000000",
                "recommendedRetryDurationMinutes": 10,
                "recommendedLivePriceSlippageTolerancePercent": 0.5,
                "estimatedDurationSeconds": 1014,
                "includedFees": [
                    {"type": "INGRESS", "chain": "Bitcoin", "asset": "BTC", "amount": "171"},
                    {"type": "EGRESS", "chain": "Ethereum", "asset": "USDC", "amount": "28146"}
                ]
            }])
        } else if path.starts_with("/api/encodeVaultSwapData") {
            let address = request.json()["destAddress"]
                .as_str()
                .unwrap()
                .to_lowercase();
            let payload = format!(
                "0103{}6400{}3201000200000000",
                address.trim_start_matches("0x"),
                "0".repeat(32)
            );
            serde_json::json!({"chain": "Bitcoin", "depositAddress": VAULT, "nulldataPayload": format!("0x{payload}")})
        } else if path.starts_with("/v2/swaps/") {
            serde_json::json!({"state": *state.lock().unwrap(), "swapEgress": {"amount": "14900000"}})
        } else {
            let bytes: Vec<u8> = VAULT.bytes().collect();
            serde_json::json!({"jsonrpc": "2.0", "id": 1, "result": {"bitcoin": [["cFLRQ", {"Btc": bytes}]]}})
        };
        bahaa_testkit::Reply::json(200, &reply)
    }
}

#[tokio::test]
async fn bitcoin_swaps_through_chainflip_deposit_into_a_listed_vault_with_our_own_data() {
    let mut ledger = Ledger::new(BITCOIN_GENESIS);
    ledger.fund(&address(SECOND), 50_000, 1);
    let ledger = Arc::new(Mutex::new(ledger));
    let url = bahaa_testkit::electrum::serve(Log::default(), Arc::clone(&ledger)).await;
    let state = Arc::new(Mutex::new("SWAPPING"));
    let side = bahaa_testkit::serve(chainflip_side(Arc::clone(&state))).await;
    let dead = url::Url::parse("http://127.0.0.1:9/").unwrap();
    let scratch = Scratch::new();
    let registry = Registry::with_networks(vec![
        bitcoin_network(url),
        common::network("ethereum", dead.clone()),
    ])
    .unwrap();
    let mut providers = bahaa_engine::swap::Providers::builtin().unwrap();
    providers.oneclick.url = dead.clone();
    providers.relay.url = dead;
    providers.chainflip.url = side.clone();
    providers.chainflip.rpc = side;
    let mut settings = config(&scratch, registry, None);
    settings.swaps = Some(providers);
    let engine = Engine::start(settings).unwrap();
    engine.create(SecretString::from("pw")).await.unwrap();
    let wallet = engine
        .import(SecretString::from(PHRASE), SecretString::from(""))
        .await
        .unwrap();
    let mut portfolio = engine.portfolio();
    until(&mut portfolio, |portfolio| !portfolio.holdings.is_empty()).await;
    let mut events = engine.events();
    let draft = engine
        .prepare_swap(bahaa_engine::SwapRequest {
            wallet,
            account: 0,
            sell: "bitcoin".parse().unwrap(),
            buy: "ethereum/0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"
                .parse()
                .unwrap(),
            amount: Spend::Exact(Amount::parse("0.0002", 8).unwrap()),
        })
        .await
        .unwrap();
    let index = draft
        .routes
        .iter()
        .position(|ranked| ranked.route.provider == "Chainflip")
        .unwrap();
    let route = &draft.routes[index].route;
    assert_eq!(route.expected.to_string(), "15");
    assert_eq!(route.minimum.to_string(), "14.85");
    let review = engine.choose_swap(draft.id, index).await.unwrap();
    assert!(!review.checked);
    assert_eq!(review.draft.transfer.to, address(VAULT));
    let tx = engine
        .confirm_send(review.draft.id, SecretString::from("pw"))
        .await
        .unwrap();
    assert_eq!(tx.to, address(VAULT));
    let raw = ledger.lock().unwrap().broadcast[0].clone();
    let minimum = format!("{:032x}", 14_850_000u128.swap_bytes());
    let ours = format!(
        "0103{}6400{minimum}3201000200000000",
        HARDHAT_0.trim_start_matches("0x")
    );
    assert!(
        raw.contains(&format!("6a30{ours}")),
        "the deposit carries our own swap data"
    );
    *state.lock().unwrap() = "COMPLETED";
    let done = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            if let Ok(Event::SwapUpdated(swap)) = events.recv().await
                && swap.status == bahaa_engine::swap::SwapStatus::Done
            {
                return swap;
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(
        (done.provider.as_str(), done.reference.as_str()),
        ("chainflip", tx.id.as_str())
    );
}
