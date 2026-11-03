#![allow(clippy::unwrap_used)]

use bahaa_chain::Wait;
use bahaa_swap::{
    Charged, Comment, CostKind, Method, Progress, Provider, Providers, Relay, Request, SwapError,
};
use bahaa_testkit::{Reply, serve};
use bahaa_types::{Address, Amount, ChainFamily};
use serde_json::{Value, json};

const ETHEREUM: &str = include_str!("fixtures/relay-ethereum.json");
const BSC_USDT: &str = include_str!("fixtures/relay-bsc-usdt.json");
const TRON: &str = include_str!("fixtures/relay-tron.json");
const TON: &str = include_str!("fixtures/relay-ton.json");
const NOW: u64 = 1_793_720_000;
const EVM: &str = "0x9858EfFD232B4033E47d90003D41EC34EcaEda94";

fn evm() -> Address {
    Address::parse(ChainFamily::Evm, EVM).unwrap()
}

fn request(
    sell: &str,
    amount: &str,
    decimals: u8,
    buy: &str,
    buy_decimals: u8,
    from: Address,
) -> Request {
    Request {
        sell: sell.parse().unwrap(),
        amount: Amount::parse(amount, decimals).unwrap(),
        buy: buy.parse().unwrap(),
        buy_decimals,
        from,
        to: evm(),
        deadline: NOW + 1_800,
    }
}

fn ether() -> Request {
    request("ethereum", "0.1", 18, "base", 18, evm())
}

async fn relay(answer: impl Fn(&Value) -> Value + Send + Sync + 'static) -> Relay {
    let url = serve(move |request| {
        if request.path().starts_with("/intents/status/v3") {
            let id = request
                .path()
                .rsplit('=')
                .next()
                .unwrap_or_default()
                .to_owned();
            return Reply::json(200, &json!({"status": id}));
        }
        Reply::json(200, &answer(&request.json()))
    })
    .await;
    let mut config = Providers::builtin().unwrap().relay;
    config.url = url;
    Relay::new(&config, reqwest::Client::new())
}

fn recorded(body: &Value) -> Value {
    let fixture = match body["originChainId"].as_u64().unwrap() {
        1 => ETHEREUM,
        56 => BSC_USDT,
        728_126_428 => TRON,
        _ => TON,
    };
    serde_json::from_str(fixture).unwrap()
}

fn request_id(fixture: &str) -> String {
    let value: Value = serde_json::from_str(fixture).unwrap();
    value["steps"][0]["requestId"].as_str().unwrap().to_owned()
}

#[tokio::test]
async fn an_ether_deposit_is_a_plain_transfer_to_the_deposit_address() {
    let relay = relay(recorded).await;
    let (route, order) = relay.quote(&ether(), NOW).await.unwrap();
    assert_eq!(order.provider, Provider::Relay);
    assert_eq!(order.reference, request_id(ETHEREUM));
    assert_eq!(
        order.deposit,
        Address::parse(
            ChainFamily::Evm,
            "0x88346a0010f9c3914b9e6d507a7200bdcdede3a5"
        )
        .unwrap()
    );
    assert_eq!(order.comment, None);
    assert_eq!(order.expected.to_string(), "0.099705500112625069");
    assert_eq!(order.minimum.to_string(), "0.097711390110372568");
    assert_eq!(order.expires_at, NOW + 30);
    assert_eq!(route.provider, "Relay");
    assert_eq!(route.method, Method::Deposit);
    assert_eq!(route.wait, Some(Wait::About(2)));
    let fee = &route.costs[0];
    assert_eq!(
        (fee.kind, fee.charged, fee.asset.to_string()),
        (
            CostKind::ProviderFee,
            Charged::InQuote,
            "ethereum".to_owned()
        )
    );
    assert_eq!(fee.amount.to_string(), "0.000294499887374931");
}

#[tokio::test]
async fn a_token_deposit_is_a_standard_token_transfer() {
    let relay = relay(recorded).await;
    let usdt = request(
        "bsc/0x55d398326f99059fF775485246999027B3197955",
        "50",
        18,
        "base/0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913",
        6,
        evm(),
    );
    let (route, order) = relay.quote(&usdt, NOW).await.unwrap();
    assert_eq!(route.expected.to_string(), "49.695136");
    assert_eq!(order.minimum.to_string(), "48.701233");
    assert_eq!(order.deposit.family(), ChainFamily::Evm);
}

#[tokio::test]
async fn a_tron_deposit_is_a_plain_trx_transfer() {
    let relay = relay(recorded).await;
    let tron = request(
        "tron",
        "100",
        6,
        "base",
        18,
        Address::parse(ChainFamily::Tron, "TUEZSdKsoDHQMeZwihtdoBiN46zxhGWYdH").unwrap(),
    );
    let (route, order) = relay.quote(&tron, NOW).await.unwrap();
    assert_eq!(route.expected.to_string(), "0.013606723822997492");
    assert_eq!(order.deposit.family(), ChainFamily::Tron);
    assert_eq!(order.reference, request_id(TRON));
}

#[tokio::test]
async fn a_ton_deposit_is_a_transfer_with_relays_comment() {
    let relay = relay(recorded).await;
    let ton = request(
        "ton",
        "10",
        9,
        "base",
        18,
        Address::parse(
            ChainFamily::Ton,
            "EQA2kCVNwVsil2EM2mB0SkXytxCqQjS4mttjDpnXmwG9T6bO",
        )
        .unwrap(),
    );
    let (route, order) = relay.quote(&ton, NOW).await.unwrap();
    assert_eq!(route.expected.to_string(), "0.005083146219559834");
    assert_eq!(
        order.deposit,
        Address::parse(
            ChainFamily::Ton,
            "EQCrdGsDTqA2t6xRR4N6V4J705F7w_VQbUdHnofsh-8lVIPs"
        )
        .unwrap()
    );
    assert!(matches!(order.comment, Some(Comment::TonBody(body)) if !body.is_empty()));
}

async fn refused(change: impl Fn(&mut Value) + Send + Sync + 'static) -> SwapError {
    let relay = relay(move |body| {
        let mut answer = recorded(body);
        change(&mut answer);
        answer
    })
    .await;
    relay.quote(&ether(), NOW).await.unwrap_err()
}

#[tokio::test]
async fn anything_but_a_plain_transfer_of_the_exact_amount_is_refused() {
    let plain =
        SwapError::Untrusted("the provider asked for something other than a plain transfer");
    let data = |answer: &mut Value| answer["steps"][0]["items"][0]["data"].take();
    assert_eq!(
        refused(move |answer| {
            let mut tx = data(answer);
            tx["to"] = json!("0xa2b53fB8d860BF379DC72d9c2a5C533CED61F341");
            answer["steps"][0]["items"][0]["data"] = tx;
        })
        .await,
        plain
    );
    assert_eq!(
        refused(move |answer| {
            let mut tx = data(answer);
            tx["value"] = json!("200000000000000000");
            answer["steps"][0]["items"][0]["data"] = tx;
        })
        .await,
        plain
    );
    assert_eq!(
        refused(move |answer| {
            let mut tx = data(answer);
            tx["data"] = json!("0x095ea7b3");
            answer["steps"][0]["items"][0]["data"] = tx;
        })
        .await,
        plain
    );
    assert_eq!(
        refused(|answer| answer["details"]["currencyIn"]["amount"] = json!("1")).await,
        SwapError::Untrusted("the quote is for another amount")
    );
    assert_eq!(
        refused(|answer| {
            let step = answer["steps"][0].clone();
            answer["steps"] = json!([step.clone(), step]);
        })
        .await,
        SwapError::Untrusted("the provider asked for more than one transaction")
    );
    assert_eq!(
        refused(|answer| {
            answer["steps"][0]["depositAddress"] = json!("TUEZSdKsoDHQMeZwihtdoBiN46zxhGWYdH");
        })
        .await,
        SwapError::Untrusted("the provider gave a deposit address this wallet can't read")
    );
}

#[tokio::test]
async fn only_origins_with_keyless_plain_deposits_are_covered() {
    let relay = relay(recorded).await;
    assert!(relay.covers(&ether()));
    assert!(!relay.covers(&request("polygon", "10", 18, "base", 18, evm())));
    assert!(!relay.covers(&request("solana", "1", 9, "base", 18, evm())));
    let ton = Address::parse(
        ChainFamily::Ton,
        "EQA2kCVNwVsil2EM2mB0SkXytxCqQjS4mttjDpnXmwG9T6bO",
    )
    .unwrap();
    assert!(!relay.covers(&request(
        "ton/EQCxE6mUtQJKFnGfaROTKOt1lZbDiiX1kCixRv7Nw2Id_sDs",
        "10",
        6,
        "base",
        18,
        ton,
    )));
    let mut to_bitcoin = ether();
    to_bitcoin.buy = "bitcoin".parse().unwrap();
    assert!(!relay.covers(&to_bitcoin));
    assert_eq!(
        relay.quote(&to_bitcoin, NOW).await.unwrap_err(),
        SwapError::Unsupported
    );
}

#[tokio::test]
async fn relay_statuses_map_to_progress() {
    let relay = relay(recorded).await;
    for (status, progress) in [
        ("waiting", Progress::Waiting),
        ("pending", Progress::Swapping),
        ("success", Progress::Done),
        ("refunded", Progress::Refunded),
    ] {
        assert_eq!(relay.status(status).await.unwrap(), progress);
    }
    assert!(matches!(
        relay.status("failure").await,
        Ok(Progress::Failed(_))
    ));
    assert!(matches!(
        relay.status("surprise").await,
        Err(SwapError::Untrusted(_))
    ));
}
