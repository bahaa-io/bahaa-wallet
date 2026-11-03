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
const POLYGON_USDT: &str = include_str!("fixtures/relay-polygon-usdt.json");
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
        native_decimals: match from.family() {
            ChainFamily::Tron => 6,
            ChainFamily::Ton | ChainFamily::Solana => 9,
            ChainFamily::Bitcoin => 8,
            ChainFamily::Evm => 18,
        },
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
    Relay::new(&config, reqwest::Client::new()).unwrap()
}

fn recorded(body: &Value) -> Value {
    let fixture = match body["originChainId"].as_u64().unwrap() {
        1 => ETHEREUM,
        56 => BSC_USDT,
        728_126_428 => TRON,
        137 => POLYGON_USDT,
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
    assert!(relay.covers(&request("polygon", "10", 18, "base", 18, evm())));
    assert!(!relay.covers(&request("avalanche", "10", 18, "base", 18, evm())));
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
    let mut to_ton_usdt = ether();
    to_ton_usdt.buy = "ton/EQCxE6mUtQJKFnGfaROTKOt1lZbDiiX1kCixRv7Nw2Id_sDs"
        .parse()
        .unwrap();
    assert!(!relay.covers(&to_ton_usdt));
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

fn polygon_usdt() -> Request {
    request(
        "polygon/0xc2132D05D31c914a87C6611C10748AEb04B58e8F",
        "20",
        6,
        "base/0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913",
        6,
        evm(),
    )
}

#[tokio::test]
async fn a_polygon_deposit_is_a_checked_call_into_relay_s_depository() {
    let relay = relay(recorded).await;
    let (route, order) = relay.quote(&polygon_usdt(), NOW).await.unwrap();
    assert_eq!(route.expected.to_string(), "19.953665");
    assert_eq!(route.minimum.to_string(), "19.554591");
    let contract = order.contract.unwrap();
    let depository = Address::parse(
        ChainFamily::Evm,
        "0x4cd00e387622c35bddb9b4c962c136462338bc31",
    )
    .unwrap();
    assert_eq!(contract.call.to, depository);
    assert_eq!(contract.spender, Some(depository));
    assert!(contract.call.value.is_zero());
    assert_eq!(contract.call.value.decimals(), 18);
    assert!(!contract.ours);
    assert_eq!(order.deposit, depository);
    assert_eq!(order.reference, request_id(POLYGON_USDT));
}

#[tokio::test]
async fn a_polygon_call_to_an_unknown_contract_or_spender_is_refused() {
    let elsewhere = |field: &'static str| {
        move |body: &Value| {
            let mut answer = recorded(body);
            let index = usize::from(field == "deposit");
            let data = &mut answer["steps"][index]["items"][0]["data"];
            if field == "deposit" {
                data["to"] = json!("0x7777777777777777777777777777777777777777");
            } else {
                let text = data["data"].as_str().unwrap().replace(
                    "4cd00e387622c35bddb9b4c962c136462338bc31",
                    "7777777777777777777777777777777777777777",
                );
                data["data"] = json!(text);
            }
            answer
        }
    };
    assert_eq!(
        relay(elsewhere("deposit"))
            .await
            .quote(&polygon_usdt(), NOW)
            .await
            .unwrap_err(),
        SwapError::Untrusted("the provider asked to call an unknown contract")
    );
    assert_eq!(
        relay(elsewhere("approve"))
            .await
            .quote(&polygon_usdt(), NOW)
            .await
            .unwrap_err(),
        SwapError::Untrusted("the provider asked to approve an unknown contract")
    );
}
