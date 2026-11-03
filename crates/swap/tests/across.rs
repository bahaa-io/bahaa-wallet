#![allow(clippy::unwrap_used)]

use bahaa_config::Registry;
use bahaa_evm::EvmChain;
use bahaa_swap::{Across, Charged, CostKind, Method, Progress, Providers, Request, SwapError};
use bahaa_testkit::{Reply, serve};
use bahaa_types::{Address, Amount, ChainFamily};
use serde_json::{Value, json};

const FEES: &str = include_str!("fixtures/across-fees.json");
const ME: &str = "0x9858EfFD232B4033E47d90003D41EC34EcaEda94";
const NOW: u64 = 1_793_729_190;
const ARBITRUM_SPOKE: &str = "0xe35e9842fceaCA96570B734083f4a58e8F7C5f2A";

fn me() -> Address {
    Address::parse(ChainFamily::Evm, ME).unwrap()
}

fn request() -> Request {
    Request {
        sell: "arbitrum/0xaf88d065e77c8cC2239327C5EDb3A432268e5831"
            .parse()
            .unwrap(),
        amount: Amount::parse("100", 6).unwrap(),
        buy: "base/0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913"
            .parse()
            .unwrap(),
        buy_decimals: 6,
        from: me(),
        to: me(),
        deadline: NOW + 1_800,
    }
}

fn arbitrum() -> EvmChain {
    let registry = Registry::builtin().unwrap();
    EvmChain::new(registry.get(&"arbitrum".parse().unwrap()).unwrap().clone()).unwrap()
}

async fn across(fees: Value) -> Across {
    let url = serve(move |request| {
        if request.path().contains("deposit/status") {
            let status = request
                .path()
                .rsplit('=')
                .next()
                .unwrap_or_default()
                .to_owned();
            return Reply::json(200, &json!({"status": status}));
        }
        Reply::json(200, &fees)
    })
    .await;
    let mut config = Providers::builtin().unwrap().across;
    config.url = url;
    Across::new(&config, reqwest::Client::new()).unwrap()
}

fn recorded() -> Value {
    serde_json::from_str(FEES).unwrap()
}

#[tokio::test]
async fn a_quote_becomes_our_own_deposit_into_the_pinned_spoke_pool() {
    let across = across(recorded()).await;
    let (route, order) = across.quote(&arbitrum(), &request(), NOW).await.unwrap();
    assert_eq!((route.provider, route.method), ("Across", Method::Escrow));
    assert_eq!(route.expected.to_string(), "99.986051");
    assert_eq!(route.minimum, route.expected);
    let fee = &route.costs[0];
    assert_eq!(
        (fee.kind, fee.charged),
        (CostKind::ProviderFee, Charged::InQuote)
    );
    assert_eq!(fee.amount.to_string(), "0.013949");
    let contract = order.contract.unwrap();
    assert_eq!(contract.call.to.to_string(), ARBITRUM_SPOKE);
    assert_eq!(contract.spender, Some(contract.call.to));
    assert!(contract.ours);
    let data = &contract.call.data;
    assert_eq!(data[..4], [0x7b, 0x93, 0x92, 0x32]);
    assert_eq!(data.len(), 4 + 13 * 32);
    let word = |index: usize| &data[4 + index * 32..4 + (index + 1) * 32];
    assert_eq!(word(0)[12..], word(1)[12..]);
    assert_eq!(word(4)[28..], 100_000_000u32.to_be_bytes());
    assert_eq!(word(5)[28..], 99_986_051u32.to_be_bytes());
    assert_eq!(word(6)[24..], 8453u64.to_be_bytes());
}

#[tokio::test]
async fn a_quote_into_another_contract_or_for_more_is_refused() {
    let mut other = recorded();
    other["spokePoolAddress"] = json!("0x09aea4b2242abC8bb4BB78D537A67a245A7bEC64");
    assert_eq!(
        across(other)
            .await
            .quote(&arbitrum(), &request(), NOW)
            .await
            .unwrap_err(),
        SwapError::Untrusted("the quote deposits into an unknown contract")
    );
    let mut more = recorded();
    more["outputAmount"] = json!("100000001");
    assert_eq!(
        across(more)
            .await
            .quote(&arbitrum(), &request(), NOW)
            .await
            .unwrap_err(),
        SwapError::Untrusted("the quote is for another amount")
    );
    let mut old = recorded();
    old["fillDeadline"] = json!("1793720000");
    assert_eq!(
        across(old)
            .await
            .quote(&arbitrum(), &request(), NOW)
            .await
            .unwrap_err(),
        SwapError::Untrusted("the quote has impossible times")
    );
    let mut small = recorded();
    small["isAmountTooLow"] = json!(true);
    assert!(matches!(
        across(small)
            .await
            .quote(&arbitrum(), &request(), NOW)
            .await,
        Err(SwapError::Refused(_))
    ));
}

#[tokio::test]
async fn only_the_same_kind_of_asset_moves_between_two_networks() {
    let across = across(recorded()).await;
    assert!(across.covers(&request()));
    let mut swap = request();
    swap.buy = "base".parse().unwrap();
    assert!(!across.covers(&swap));
    let mut here = request();
    here.buy = "arbitrum/0xFd086bC7CD5C481DCC9C85ebE478A1C0b69FCbb9"
        .parse()
        .unwrap();
    assert!(!across.covers(&here));
    let mut bsc = request();
    bsc.sell = "bsc/0x8AC76a51cc950d9822D68b83fE1Ad97B32Cd580d"
        .parse()
        .unwrap();
    assert!(!across.covers(&bsc));
}

#[tokio::test]
async fn deposit_statuses_map_to_progress() {
    let across = across(recorded()).await;
    let network = "arbitrum".parse().unwrap();
    for (status, progress) in [
        ("pending", Progress::Swapping),
        ("filled", Progress::Done),
        ("refunded", Progress::Refunded),
    ] {
        assert_eq!(across.status(&network, status).await.unwrap(), progress);
    }
    assert!(matches!(
        across.status(&network, "expired").await,
        Ok(Progress::Failed(_))
    ));
    assert!(across.status(&network, "surprise").await.is_err());
}
