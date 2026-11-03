#![allow(clippy::unwrap_used)]

use bahaa_chain::Chain;
use bahaa_chain::ContractSwap;
use bahaa_chain::now;
use bahaa_config::Registry;
use bahaa_crypto::{Mnemonic, WordCount};
use bahaa_evm::EvmChain;
use bahaa_solana::SolanaChain;
use bahaa_swap::{Cow, Jupiter, Method, OneClick, Providers, Relay, Request, StonFi, SwapError};
use bahaa_ton::TonChain;
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

#[tokio::test]
#[ignore = "talks to CoW"]
async fn live_cow_accepts_our_signature_and_only_misses_the_balance() {
    let registry = Registry::builtin().unwrap();
    let chain = EvmChain::new(registry.get(&"bsc".parse().unwrap()).unwrap().clone()).unwrap();
    let seed = Mnemonic::generate(WordCount::Twelve).unwrap().to_seed("");
    let me = chain.derive(&seed, 0).unwrap();
    let cow = Cow::new(
        &Providers::builtin().unwrap().cow,
        bahaa_chain::http::client().unwrap(),
    )
    .unwrap();
    let asked = request(
        "bsc/0x55d398326f99059fF775485246999027B3197955",
        "50",
        18,
        "bsc",
        18,
        me,
        me,
    );
    let (route, order) = cow.quote(&asked, now()).await.unwrap();
    assert!(route.minimum < route.expected);
    let typed = order.typed.unwrap();
    assert_eq!(order.reference, typed.uid());
    let signature = chain.sign_typed(&typed.digest(), &seed, 0, &me).unwrap();
    let bsc = "bsc".parse().unwrap();
    let refused = cow
        .place(&bsc, &typed.body(&signature).to_string(), &typed.uid())
        .await
        .unwrap_err();
    let SwapError::Refused(reason) = refused else {
        panic!("{refused:?}");
    };
    let mut forged = signature;
    forged[10] ^= 1;
    let other = cow
        .place(&bsc, &typed.body(&forged).to_string(), &typed.uid())
        .await
        .unwrap_err();
    assert_ne!(other, SwapError::Refused(reason.clone()), "{other:?}");
    let lower = reason.to_lowercase();
    assert!(!lower.contains("signature"), "{reason}");
    assert!(
        ["balance", "allowance", "funds"]
            .iter()
            .any(|word| lower.contains(word)),
        "{reason}"
    );
}

#[tokio::test]
#[ignore = "talks to Jupiter and Solana"]
async fn live_jupiter_routes_pass_the_solana_check_for_a_funded_address() {
    let registry = Registry::builtin().unwrap();
    let chain =
        SolanaChain::new(registry.get(&"solana".parse().unwrap()).unwrap().clone()).unwrap();
    let whale = Address::parse(
        ChainFamily::Solana,
        "9WzDXwBbmkg8ZTbNMqUxvQRAyrZzDsGYdLVL9zYtAWWM",
    )
    .unwrap();
    let jupiter = Jupiter::new(
        &Providers::builtin().unwrap().jupiter,
        bahaa_chain::http::client().unwrap(),
    )
    .unwrap();
    let asked = request(
        "solana",
        "0.1",
        9,
        "solana/EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v",
        6,
        whale,
        whale,
    );
    let (route, order) = jupiter.quote(&chain, &asked, now()).await.unwrap();
    assert_eq!(
        (route.provider, route.method),
        ("Jupiter", Method::Aggregator)
    );
    assert!(route.minimum < route.expected);
    let contract = order.contract.unwrap();
    let swap = ContractSwap {
        account: 0,
        from: whale,
        call: contract.call,
        spender: None,
        sell: asked.sell.clone(),
        amount: asked.amount,
        buy: Some((asked.buy.clone(), route.minimum)),
        ours: false,
        spent: None,
    };
    let (plans, fee) = chain.prepare_swap(&swap).await.unwrap();
    assert_eq!(plans.len(), 1);
    assert!(fee.expected() < Amount::parse("0.01", 9).unwrap());
    let mut greedy = swap.clone();
    greedy.buy = Some((
        asked.buy.clone(),
        route.expected.checked_add(route.expected).unwrap(),
    ));
    assert_eq!(
        chain.prepare_swap(&greedy).await.unwrap_err().to_string(),
        "the swap would pay less than its minimum"
    );
}

#[tokio::test]
#[ignore = "talks to STON.fi and TON"]
async fn live_stonfi_routes_name_the_pools_the_chain_confirms() {
    let registry = Registry::builtin().unwrap();
    let chain = TonChain::new(registry.get(&"ton".parse().unwrap()).unwrap().clone()).unwrap();
    let stonfi = StonFi::new(
        &Providers::builtin().unwrap().stonfi,
        bahaa_chain::http::client().unwrap(),
    )
    .unwrap();
    let owner = Address::parse(
        ChainFamily::Ton,
        "UQCi1ivY6QriyKIQZrSTwhvf4qXI19qIPe4XAOYo-qMMUEiR",
    )
    .unwrap();
    let usdt = "ton/EQCxE6mUtQJKFnGfaROTKOt1lZbDiiX1kCixRv7Nw2Id_sDs";
    for (sell, amount, decimals, buy, buy_decimals) in
        [("ton", "1", 9, usdt, 6), (usdt, "1", 6, "ton", 9)]
    {
        let mut asked = request(sell, amount, decimals, buy, buy_decimals, owner, owner);
        asked.native_decimals = 9;
        let (route, order) = stonfi.quote(&chain, &asked, now()).await.unwrap();
        assert_eq!((route.provider, route.method), ("STON.fi", Method::Pool));
        assert!(route.minimum < route.expected);
        let contract = order.contract.unwrap();
        assert!(contract.ours);
        let swap = ContractSwap {
            account: 0,
            from: owner,
            call: contract.call,
            spender: None,
            sell: asked.sell.clone(),
            amount: asked.amount,
            buy: Some((asked.buy.clone(), route.minimum)),
            ours: true,
            spent: contract.spent,
        };
        match chain.prepare_swap(&swap).await {
            Ok((plans, fee)) => {
                assert_eq!(plans.len(), 1);
                assert!(fee.expected() <= fee.max());
            }
            Err(bahaa_chain::ChainError::Insufficient { .. }) => {}
            Err(other) => panic!("{other:?}"),
        }
    }
}
