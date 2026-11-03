#![allow(clippy::unwrap_used)]

use std::collections::BTreeSet;

use bahaa_chain::Wait;
use bahaa_config::Registry;
use bahaa_swap::{
    Charged, CostKind, Method, OneClick, OneClickConfig, Progress, Provider, Providers, Request,
    SwapError,
};
use bahaa_testkit::{Reply, serve};
use bahaa_types::{Address, Amount, AssetId, ChainFamily};
use serde_json::{Value, json};

const LIVE: &str = include_str!("fixtures/oneclick-live.json");
const DRY: &str = include_str!("fixtures/oneclick-dry.json");
const STATUS: &str = include_str!("fixtures/oneclick-status.json");
const DEADLINE: u64 = 1_793_722_541;
const NOW: u64 = DEADLINE - 1_800;

fn request() -> Request {
    Request {
        sell: "tron".parse().unwrap(),
        amount: Amount::parse("400", 6).unwrap(),
        buy: "ethereum".parse().unwrap(),
        buy_decimals: 18,
        from: Address::parse(ChainFamily::Tron, "TUEZSdKsoDHQMeZwihtdoBiN46zxhGWYdH").unwrap(),
        to: Address::parse(
            ChainFamily::Evm,
            "0x9858EfFD232B4033E47d90003D41EC34EcaEda94",
        )
        .unwrap(),
        deadline: DEADLINE,
    }
}

async fn provider(live: String, dry: String, status: String) -> OneClick {
    let url = serve(move |request| {
        let path = request.path().to_owned();
        if path.starts_with("/status") {
            return Reply::json(200, &serde_json::from_str::<Value>(&status).unwrap());
        }
        if path == "/deposit/submit" {
            return Reply::json(200, &json!({}));
        }
        let body = if request.json()["dry"] == Value::Bool(false) {
            &live
        } else {
            &dry
        };
        Reply::json(201, &serde_json::from_str::<Value>(body).unwrap())
    })
    .await;
    let mut config: OneClickConfig = Providers::builtin().unwrap().oneclick;
    config.url = url;
    OneClick::new(&config, reqwest::Client::new()).unwrap()
}

async fn recorded() -> OneClick {
    provider(LIVE.to_owned(), DRY.to_owned(), STATUS.to_owned()).await
}

#[tokio::test]
async fn a_signed_order_gives_the_deposit_address_and_exact_amounts() {
    let oneclick = recorded().await;
    let (route, order) = oneclick.quote(&request(), true, NOW).await.unwrap();
    let order = order.unwrap();
    assert_eq!(order.provider, Provider::OneClick);
    assert_eq!(order.reference, "THQiLhFPYUAohMsJTaPVVRMfZwr6BwdUTa");
    assert_eq!(
        order.deposit,
        Address::parse(ChainFamily::Tron, "THQiLhFPYUAohMsJTaPVVRMfZwr6BwdUTa").unwrap()
    );
    assert_eq!(order.comment, None);
    assert_eq!(order.expected.to_string(), "0.05571973751485288");
    assert_eq!(order.minimum.to_string(), "0.055162540139704351");
    assert_eq!(order.expires_at, NOW + 30);
    assert_eq!(route.provider, "NEAR Intents");
    assert_eq!(route.method, Method::Deposit);
    assert_eq!(route.expected, order.expected);
    assert_eq!(route.wait, Some(Wait::About(87)));
    let fee = &route.costs[0];
    assert_eq!(fee.kind, CostKind::ProviderFee);
    assert_eq!(fee.charged, Charged::InQuote);
    assert_eq!(fee.amount.to_string(), "0.000035");
}

#[tokio::test]
async fn a_dry_quote_is_signed_too_but_has_no_order() {
    let oneclick = recorded().await;
    let (route, order) = oneclick.quote(&request(), false, NOW).await.unwrap();
    assert!(order.is_none());
    assert_eq!(route.expected.to_string(), "0.055717183986366752");
}

#[tokio::test]
async fn an_order_with_another_deposit_address_fails_the_signature() {
    let forged = LIVE.replace(
        "THQiLhFPYUAohMsJTaPVVRMfZwr6BwdUTa",
        "TUEZSdKsoDHQMeZwihtdoBiN46zxhGWYdH",
    );
    let oneclick = provider(forged, DRY.to_owned(), STATUS.to_owned()).await;
    assert_eq!(
        oneclick.quote(&request(), true, NOW).await.unwrap_err(),
        SwapError::Untrusted("the quote is not signed by NEAR Intents")
    );
}

#[tokio::test]
async fn a_raised_amount_fails_the_signature() {
    let forged = DRY.replace("55717183986366752", "95717183986366752");
    let oneclick = provider(LIVE.to_owned(), forged, STATUS.to_owned()).await;
    assert_eq!(
        oneclick.quote(&request(), false, NOW).await.unwrap_err(),
        SwapError::Untrusted("the quote is not signed by NEAR Intents")
    );
}

#[tokio::test]
async fn a_signed_quote_for_another_recipient_is_refused() {
    let oneclick = recorded().await;
    let mut mine = request();
    mine.to = Address::parse(
        ChainFamily::Evm,
        "0xa2b53fB8d860BF379DC72d9c2a5C533CED61F341",
    )
    .unwrap();
    assert_eq!(
        oneclick.quote(&mine, true, NOW).await.unwrap_err(),
        SwapError::Untrusted("the quote is for another request")
    );
}

#[tokio::test]
async fn a_refusal_carries_the_providers_reason() {
    let url = serve(|_| {
        Reply::json(
            400,
            &json!({"message": "Quoting for this pair is not available\u{202e}"}),
        )
    })
    .await;
    let mut config = Providers::builtin().unwrap().oneclick;
    config.url = url;
    let oneclick = OneClick::new(&config, reqwest::Client::new()).unwrap();
    assert_eq!(
        oneclick.quote(&request(), false, NOW).await.unwrap_err(),
        SwapError::Refused("Quoting for this pair is not available".to_owned())
    );
}

#[tokio::test]
async fn statuses_map_to_progress() {
    let oneclick = recorded().await;
    assert_eq!(
        oneclick
            .status("THQiLhFPYUAohMsJTaPVVRMfZwr6BwdUTa")
            .await
            .unwrap(),
        Progress::Waiting
    );
    for (status, progress) in [
        ("KNOWN_DEPOSIT_TX", Progress::Received),
        ("PROCESSING", Progress::Swapping),
        ("SUCCESS", Progress::Done),
        ("REFUNDED", Progress::Refunded),
    ] {
        let reply = json!({"status": status}).to_string();
        let oneclick = provider(LIVE.to_owned(), DRY.to_owned(), reply).await;
        assert_eq!(oneclick.status("x").await.unwrap(), progress);
    }
    let odd = provider(
        LIVE.to_owned(),
        DRY.to_owned(),
        json!({"status": "MAYBE"}).to_string(),
    )
    .await;
    assert!(matches!(
        odd.status("x").await,
        Err(SwapError::Untrusted(_))
    ));
    oneclick.submit("x", "0xabc").await.unwrap();
}

#[test]
fn every_builtin_mainnet_asset_has_one_mapping_and_nothing_else_is_mapped() {
    let registry = Registry::builtin().unwrap();
    let ours: BTreeSet<AssetId> = registry
        .networks()
        .filter(|network| !network.testnet)
        .flat_map(|network| network.assets().map(|(asset, _)| asset))
        .collect();
    let providers = Providers::builtin().unwrap();
    let mapped: BTreeSet<AssetId> = providers.oneclick.assets.keys().cloned().collect();
    assert_eq!(mapped, ours);
    let ids: BTreeSet<&String> = providers.oneclick.assets.values().collect();
    assert_eq!(ids.len(), mapped.len());
    for network in providers.relay.chains.keys() {
        assert!(
            registry
                .get(network)
                .is_some_and(|network| !network.testnet)
        );
    }
}
