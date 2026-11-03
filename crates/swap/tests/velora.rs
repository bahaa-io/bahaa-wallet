#![allow(clippy::unwrap_used)]

use bahaa_config::Registry;
use bahaa_evm::EvmChain;
use bahaa_swap::{Charged, CostKind, Method, Providers, Request, SwapError, Velora};
use bahaa_testkit::{Reply, serve};
use bahaa_types::{Address, Amount, ChainFamily};
use serde_json::Value;

const PRICES: &str = include_str!("fixtures/velora-prices.json");
const BUILD: &str = include_str!("fixtures/velora-build.json");
const ME: &str = "0x9858EfFD232B4033E47d90003D41EC34EcaEda94";
const NOW: u64 = 1_793_720_000;

fn request() -> Request {
    let me = Address::parse(ChainFamily::Evm, ME).unwrap();
    Request {
        sell: "bsc".parse().unwrap(),
        amount: Amount::parse("0.1", 18).unwrap(),
        buy: "bsc/0x55d398326f99059fF775485246999027B3197955"
            .parse()
            .unwrap(),
        buy_decimals: 18,
        native_decimals: 18,
        from: me,
        to: me,
        deadline: NOW + 1_800,
    }
}

fn bsc() -> EvmChain {
    let registry = Registry::builtin().unwrap();
    EvmChain::new(registry.get(&"bsc".parse().unwrap()).unwrap().clone()).unwrap()
}

async fn provider(build: String) -> Velora {
    let url = serve(move |request| {
        let body = if request.path().starts_with("/transactions/56") {
            build.clone()
        } else {
            assert!(
                request
                    .path()
                    .contains("includeContractMethods=swapExactAmountIn")
            );
            PRICES.to_owned()
        };
        Reply::json(200, &serde_json::from_str::<Value>(&body).unwrap())
    })
    .await;
    let mut config = Providers::builtin().unwrap().velora;
    config.url = url;
    Velora::new(&config, reqwest::Client::new()).unwrap()
}

#[tokio::test]
async fn a_recorded_route_shows_velora_s_fee_and_a_minimum_after_it() {
    let velora = provider(BUILD.to_owned()).await;
    let (route, order) = velora.quote(&bsc(), &request(), NOW).await.unwrap();
    assert_eq!(
        (route.provider, route.method),
        ("Velora", Method::Aggregator)
    );
    assert_eq!(route.expected.to_string(), "69.730646482651004784");
    assert_eq!(route.minimum.to_string(), "69.38199325023774976");
    let fee = &route.costs[0];
    assert_eq!(
        (fee.kind, fee.charged),
        (CostKind::ProviderFee, Charged::InQuote)
    );
    assert_eq!(fee.amount.to_string(), "0.006973762024467548");
    let contract = order.contract.unwrap();
    assert_eq!(
        contract.call.to.to_string().to_lowercase(),
        "0x6a000f20005980200259b80c5102003040001068"
    );
    assert_eq!(contract.call.value.to_string(), "0.1");
}

#[tokio::test]
async fn a_route_paying_someone_else_or_less_is_refused() {
    let elsewhere = BUILD.replace("9858effd232b4033e47d90003d41ec34ecaeda94", &"77".repeat(20));
    assert_eq!(
        provider(elsewhere)
            .await
            .quote(&bsc(), &request(), NOW)
            .await
            .unwrap_err(),
        SwapError::Untrusted("the route pays someone else or less than it shows")
    );
    let lower = BUILD.replace("3c2f702256a2581fa", "0c2f702256a2581fa");
    assert_eq!(
        provider(lower)
            .await
            .quote(&bsc(), &request(), NOW)
            .await
            .unwrap_err(),
        SwapError::Untrusted("the route pays someone else or less than it shows")
    );
    let other = BUILD.replace(
        "0x6a000f20005980200259b80c5102003040001068",
        "0x6a000f20005980200259b80c5102003040001000",
    );
    assert_eq!(
        provider(other)
            .await
            .quote(&bsc(), &request(), NOW)
            .await
            .unwrap_err(),
        SwapError::Untrusted("the route calls an unknown contract")
    );
}
