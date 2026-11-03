#![allow(clippy::unwrap_used)]

use bahaa_chain::now;
use bahaa_swap::{OneClick, Providers, Relay, Request};
use bahaa_types::{Address, Amount, ChainFamily};

fn evm() -> Address {
    Address::parse(
        ChainFamily::Evm,
        "0x9858EfFD232B4033E47d90003D41EC34EcaEda94",
    )
    .unwrap()
}

fn ton() -> Address {
    Address::parse(
        ChainFamily::Ton,
        "EQA2kCVNwVsil2EM2mB0SkXytxCqQjS4mttjDpnXmwG9T6bO",
    )
    .unwrap()
}

fn request(
    sell: &str,
    amount: &str,
    decimals: u8,
    buy: &str,
    buy_decimals: u8,
    from: Address,
    to: Address,
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
        to,
        deadline: now() + 1_800,
    }
}

#[tokio::test]
#[ignore = "talks to NEAR Intents"]
async fn live_near_intents_signs_quotes_for_ton_and_tron() {
    let providers = Providers::builtin().unwrap();
    let oneclick =
        OneClick::new(&providers.oneclick, bahaa_chain::http::client().unwrap()).unwrap();
    let tron = Address::parse(ChainFamily::Tron, "TUEZSdKsoDHQMeZwihtdoBiN46zxhGWYdH").unwrap();
    for asked in [
        request("ton", "10", 9, "ethereum", 18, ton(), evm()),
        request("ethereum", "0.01", 18, "ton", 9, evm(), ton()),
        request("tron", "400", 6, "ethereum", 18, tron, evm()),
    ] {
        let (route, _) = oneclick.quote(&asked, false, now()).await.unwrap();
        assert!(!route.expected.is_zero());
        assert!(route.minimum.units() <= route.expected.units());
    }
}

#[tokio::test]
#[ignore = "talks to Relay"]
async fn live_relay_gives_plain_deposits_from_ethereum_and_ton() {
    let providers = Providers::builtin().unwrap();
    let relay = Relay::new(&providers.relay, bahaa_chain::http::client().unwrap()).unwrap();
    for asked in [
        request("ethereum", "0.1", 18, "base", 18, evm(), evm()),
        request("ton", "10", 9, "base", 18, ton(), evm()),
    ] {
        let (route, order) = relay.quote(&asked, now()).await.unwrap();
        assert_eq!(route.expected, order.expected);
        assert_ne!(order.deposit, asked.from);
    }
}
