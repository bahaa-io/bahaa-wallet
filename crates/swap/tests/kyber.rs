#![allow(clippy::unwrap_used)]

use bahaa_config::Registry;
use bahaa_evm::EvmChain;
use bahaa_swap::{KyberSwap, Method, Providers, Request, SwapError};
use bahaa_testkit::{Reply, serve};
use bahaa_types::{Address, Amount, ChainFamily};
use serde_json::Value;

const ROUTES: &str = include_str!("fixtures/kyber-routes.json");
const BUILD: &str = include_str!("fixtures/kyber-build.json");
const ME: &str = "0x9858EfFD232B4033E47d90003D41EC34EcaEda94";
const NOW: u64 = 1_793_720_000;

fn me() -> Address {
    Address::parse(ChainFamily::Evm, ME).unwrap()
}

fn request() -> Request {
    Request {
        sell: "bsc".parse().unwrap(),
        amount: Amount::parse("0.1", 18).unwrap(),
        buy: "bsc/0x55d398326f99059fF775485246999027B3197955"
            .parse()
            .unwrap(),
        buy_decimals: 18,
        native_decimals: 18,
        from: me(),
        to: me(),
        deadline: NOW + 1_800,
    }
}

fn bsc() -> EvmChain {
    let registry = Registry::builtin().unwrap();
    EvmChain::new(registry.get(&"bsc".parse().unwrap()).unwrap().clone()).unwrap()
}

async fn provider(routes: String, build: String) -> KyberSwap {
    let url = serve(move |request| {
        let body = if request.path().contains("/route/build") {
            &build
        } else {
            assert!(request.path().contains("excludeRFQSources=true"));
            &routes
        };
        Reply::json(200, &serde_json::from_str::<Value>(body).unwrap())
    })
    .await;
    let mut config = Providers::builtin().unwrap().kyberswap;
    config.url = url;
    KyberSwap::new(&config, reqwest::Client::new()).unwrap()
}

#[tokio::test]
async fn a_recorded_route_becomes_a_checked_call_to_the_pinned_router() {
    let kyber = provider(ROUTES.to_owned(), BUILD.to_owned()).await;
    let (route, order) = kyber.quote(&bsc(), &request(), NOW).await.unwrap();
    assert_eq!(
        (route.provider, route.method),
        ("KyberSwap", Method::Aggregator)
    );
    assert_eq!(route.expected.to_string(), "69.689991780775332218");
    assert_eq!(route.minimum.to_string(), "69.341541821871455556");
    let contract = order.contract.unwrap();
    assert_eq!(
        contract.call.to.to_string(),
        "0x6131B5fae19EA4f9D964eAc0408E4408b66337b5"
    );
    assert_eq!(contract.call.value.to_string(), "0.1");
    assert_eq!(contract.spender, None);
    assert!(!contract.ours);
}

#[tokio::test]
async fn a_route_paying_someone_else_less_or_through_another_contract_is_refused() {
    let elsewhere = BUILD.replace("9858effd232b4033e47d90003d41ec34ecaeda94", &"77".repeat(20));
    let kyber = provider(ROUTES.to_owned(), elsewhere).await;
    assert_eq!(
        kyber.quote(&bsc(), &request(), NOW).await.unwrap_err(),
        SwapError::Untrusted("the route pays someone else or less than it shows")
    );
    assert_eq!(BUILD.matches("3c24ea4e6568a7d44").count(), 1);
    let lower = BUILD.replace("3c24ea4e6568a7d44", "0c24ea4e6568a7d44");
    let kyber = provider(ROUTES.to_owned(), lower).await;
    assert_eq!(
        kyber.quote(&bsc(), &request(), NOW).await.unwrap_err(),
        SwapError::Untrusted("the route pays someone else or less than it shows")
    );
    let other = BUILD.replace(
        "0x6131B5fae19EA4f9D964eAc0408E4408b66337b5",
        "0x6131b5fae19ea4f9d964eac0408e4408b6633700",
    );
    let kyber = provider(ROUTES.to_owned(), other).await;
    assert_eq!(
        kyber.quote(&bsc(), &request(), NOW).await.unwrap_err(),
        SwapError::Untrusted("the route calls an unknown contract")
    );
    let mut bigger = request();
    bigger.amount = Amount::parse("0.2", 18).unwrap();
    let kyber = provider(ROUTES.to_owned(), BUILD.to_owned()).await;
    assert_eq!(
        kyber.quote(&bsc(), &bigger, NOW).await.unwrap_err(),
        SwapError::Untrusted("the route is for another request")
    );
}

#[test]
fn only_same_network_evm_swaps_are_covered() {
    let config = Providers::builtin().unwrap().kyberswap;
    let kyber = KyberSwap::new(&config, reqwest::Client::new()).unwrap();
    assert!(kyber.covers(&request()));
    let mut across = request();
    across.buy = "base".parse().unwrap();
    assert!(!kyber.covers(&across));
    let mut avalanche = request();
    avalanche.sell = "avalanche".parse().unwrap();
    avalanche.buy = "avalanche/0xB97EF9Ef8734C71904D8002F8b6Bc66Dd9c48a6E"
        .parse()
        .unwrap();
    assert!(!kyber.covers(&avalanche));
}
