#![allow(clippy::unwrap_used)]

use bahaa_swap::{Method, Providers, Request, SwapError, Thorchain};
use bahaa_testkit::{Reply, serve};
use bahaa_types::{Address, Amount, ChainFamily};
use serde_json::Value;

const INBOUND: &str = include_str!("fixtures/thorchain-inbound.json");
const QUOTE: &str = include_str!("fixtures/thorchain-quote.json");
const NOW: u64 = 1_793_735_000;

async fn thorchain() -> Thorchain {
    let inbound: Value = serde_json::from_str(INBOUND).unwrap();
    let quote: Value = serde_json::from_str(QUOTE).unwrap();
    let url = serve(move |request| {
        if request.path().contains("inbound_addresses") {
            Reply::json(200, &inbound)
        } else {
            Reply::json(200, &quote)
        }
    })
    .await;
    let mut config = Providers::builtin().unwrap().thorchain;
    config.url = url;
    Thorchain::new(&config, reqwest::Client::new()).unwrap()
}

fn request(amount: &str) -> Request {
    Request {
        sell: "base".parse().unwrap(),
        amount: Amount::parse(amount, 18).unwrap(),
        buy: "bitcoin".parse().unwrap(),
        buy_decimals: 8,
        native_decimals: 18,
        from: Address::parse(
            ChainFamily::Evm,
            "0x9858effd232b4033e47d90003d41ec34ecaeda94",
        )
        .unwrap(),
        to: Address::parse(
            ChainFamily::Bitcoin,
            "bc1qcr8te4kr609gcawutmrza0j4xv80jy8z306fyu",
        )
        .unwrap(),
        deadline: NOW + 1_800,
    }
}

#[tokio::test]
async fn a_recorded_quote_becomes_a_router_deposit_with_our_own_limit() {
    let (route, order) = thorchain().await.quote(&request("0.1"), NOW).await.unwrap();
    assert_eq!(route.method, Method::Validators);
    assert_eq!(route.expected.to_string(), "0.00292405");
    assert_eq!(route.minimum.to_string(), "0.0028948");
    let memo = String::from_utf8_lossy(&order.contract.unwrap().call.data).into_owned();
    assert!(
        memo.contains("=:b:bc1qcr8te4kr609gcawutmrza0j4xv80jy8z306fyu:289480/0/9"),
        "{memo}"
    );
}

#[tokio::test]
async fn an_amount_under_the_recommended_minimum_is_refused() {
    let refused = thorchain()
        .await
        .quote(&request("0.001"), NOW)
        .await
        .unwrap_err();
    assert!(
        matches!(&refused, SwapError::Refused(text) if text.contains("at least 0.00261555 ETH")),
        "{refused}"
    );
}
