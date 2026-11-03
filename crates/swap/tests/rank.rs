#![allow(clippy::unwrap_used)]

use std::collections::HashMap;

use bahaa_chain::Wait;
use bahaa_swap::{Caution, Charged, Cost, CostKind, Method, Ranked, Refusal, Route, rank};
use bahaa_types::{Amount, AssetId, Price};

const ETH: &str = "ethereum";
const ETH_USDC: &str = "ethereum/0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48";
const TRX: &str = "tron";
const TRON_USDT: &str = "tron/TR7NHqjeKQxGTCi8q8ZY4pL8otSzgjLj6t";
const BTC: &str = "bitcoin";
const BASE_ETH: &str = "base";
const SOL: &str = "solana";
const SOL_USDC: &str = "solana/EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v";
const GRAM: &str = "ton";
const TON_USDT: &str = "ton/EQCxE6mUtQJKFnGfaROTKOt1lZbDiiX1kCixRv7Nw2Id_sDs";

fn asset(text: &str) -> AssetId {
    text.parse().unwrap()
}

fn units(text: &str, decimals: u8) -> Amount {
    Amount::parse(text, decimals).unwrap()
}

fn prices(table: &[(&str, &str)]) -> impl Fn(&AssetId) -> Option<Price> {
    let known: HashMap<AssetId, Price> = table
        .iter()
        .map(|(id, price)| (asset(id), Price::parse(price).unwrap()))
        .collect();
    move |id| known.get(id).copied()
}

fn cost(kind: CostKind, id: &str, amount: &str, decimals: u8, charged: Charged) -> Cost {
    Cost {
        kind,
        asset: asset(id),
        amount: units(amount, decimals),
        charged,
    }
}

struct Quote {
    provider: &'static str,
    method: Method,
    expected: &'static str,
    minimum: &'static str,
    costs: Vec<Cost>,
}

fn routes(sell: (&str, &str, u8), buy: (&str, u8), quotes: Vec<Quote>) -> Vec<Route> {
    quotes
        .into_iter()
        .map(|quote| Route {
            provider: quote.provider,
            method: quote.method,
            sell: asset(sell.0),
            amount: units(sell.1, sell.2),
            buy: asset(buy.0),
            expected: units(quote.expected, buy.1),
            minimum: units(quote.minimum, buy.1),
            costs: quote.costs,
            wait: Some(Wait::About(30)),
        })
        .collect()
}

fn providers(ranked: &[Ranked]) -> Vec<&'static str> {
    ranked.iter().map(|ranked| ranked.route.provider).collect()
}

fn net(ranked: &Ranked) -> String {
    ranked.value.as_ref().unwrap().net.to_string()
}

fn gas(eth: &str) -> Cost {
    cost(CostKind::NetworkFee, ETH, eth, 18, Charged::Extra)
}

#[test]
fn one_eth_to_usdc_ranks_by_what_arrives_after_gas() {
    let ranked = rank(
        routes(
            (ETH, "1", 18),
            (ETH_USDC, 6),
            vec![
                Quote {
                    provider: "KyberSwap",
                    method: Method::Aggregator,
                    expected: "2285.819443",
                    minimum: "2274.390345",
                    costs: vec![gas("0.00023755823256171")],
                },
                Quote {
                    provider: "Uniswap",
                    method: Method::Pool,
                    expected: "2284.891276",
                    minimum: "2273.466819",
                    costs: vec![gas("0.0000282440919")],
                },
                Quote {
                    provider: "LI.FI",
                    method: Method::Aggregator,
                    expected: "2279.800483",
                    minimum: "2264.931624",
                    costs: vec![gas("0.0000376587892")],
                },
                Quote {
                    provider: "CoW Swap",
                    method: Method::Auction,
                    expected: "2285.488771",
                    minimum: "2274.061327",
                    costs: vec![cost(
                        CostKind::ProviderFee,
                        ETH,
                        "0.000106785490816858",
                        18,
                        Charged::InQuote,
                    )],
                },
            ],
        ),
        prices(&[(ETH, "2285"), (ETH_USDC, "1")]),
        2,
    );
    assert_eq!(
        providers(&ranked),
        ["CoW Swap", "KyberSwap", "Uniswap", "LI.FI"]
    );
    assert_eq!(
        ranked.iter().map(net).collect::<Vec<_>>(),
        ["2285.49", "2285.28", "2284.83", "2279.71"]
    );
    assert!(ranked.iter().all(|ranked| ranked.cautions.is_empty()));
}

#[test]
fn a_small_tron_swap_counts_the_energy_it_burns() {
    let energy = |trx: &str| cost(CostKind::NetworkFee, TRX, trx, 6, Charged::Extra);
    let ranked = rank(
        routes(
            (TRX, "100", 6),
            (TRON_USDT, 6),
            vec![
                Quote {
                    provider: "SunSwap",
                    method: Method::Pool,
                    expected: "32.15802",
                    minimum: "31.997229",
                    costs: vec![energy("15.7354")],
                },
                Quote {
                    provider: "SunSwap router",
                    method: Method::Aggregator,
                    expected: "32.228447",
                    minimum: "32.067305",
                    costs: vec![energy("15.7354")],
                },
                Quote {
                    provider: "Relay",
                    method: Method::Escrow,
                    expected: "31.15501",
                    minimum: "30.843459",
                    costs: vec![
                        energy("6.855208"),
                        cost(
                            CostKind::ProviderFee,
                            TRON_USDT,
                            "1.074917",
                            6,
                            Charged::InQuote,
                        ),
                    ],
                },
            ],
        ),
        prices(&[(TRX, "0.322"), (TRON_USDT, "1")]),
        2,
    );
    assert_eq!(providers(&ranked), ["Relay", "SunSwap router", "SunSwap"]);
    assert_eq!(
        ranked.iter().map(net).collect::<Vec<_>>(),
        ["28.95", "27.16", "27.09"]
    );
    assert_eq!(
        ranked[0].cautions,
        [Caution::BelowMarket(322), Caution::CostlyFees(686)]
    );
    assert_eq!(ranked[1].cautions, [Caution::CostlyFees(1574)]);
}

#[test]
fn bitcoin_to_ether_prefers_the_better_payout() {
    let send = || cost(CostKind::NetworkFee, BTC, "0.00000141", 8, Charged::Extra);
    let ranked = rank(
        routes(
            (BTC, "0.01", 8),
            (ETH, 18),
            vec![
                Quote {
                    provider: "1Click",
                    method: Method::Deposit,
                    expected: "0.337191620738380234",
                    minimum: "0.333819704530996431",
                    costs: vec![send()],
                },
                Quote {
                    provider: "Chainflip",
                    method: Method::Validators,
                    expected: "0.338162868187233709",
                    minimum: "0.329708796482552866",
                    costs: vec![send()],
                },
            ],
        ),
        prices(&[(BTC, "77259"), (ETH, "2285")]),
        2,
    );
    assert_eq!(providers(&ranked), ["Chainflip", "1Click"]);
    assert_eq!(
        ranked.iter().map(net).collect::<Vec<_>>(),
        ["772.59", "770.37"]
    );
}

#[test]
fn moving_ether_to_base_picks_the_cheapest_relayer() {
    let ranked = rank(
        routes(
            (ETH, "0.1", 18),
            (BASE_ETH, 18),
            vec![
                Quote {
                    provider: "Relay",
                    method: Method::Escrow,
                    expected: "0.099920480910714471",
                    minimum: "0.099420878506160898",
                    costs: vec![gas("0.0000264341")],
                },
                Quote {
                    provider: "Across",
                    method: Method::Escrow,
                    expected: "0.099988532783548586",
                    minimum: "0.099988532783548586",
                    costs: vec![
                        gas("0.00001412204595"),
                        cost(
                            CostKind::ProviderFee,
                            BASE_ETH,
                            "0.000011467216451414",
                            18,
                            Charged::InQuote,
                        ),
                    ],
                },
            ],
        ),
        prices(&[(ETH, "2285"), (BASE_ETH, "2285")]),
        2,
    );
    assert_eq!(providers(&ranked), ["Across", "Relay"]);
    assert_eq!(
        ranked.iter().map(net).collect::<Vec<_>>(),
        ["228.44", "228.26"]
    );
}

#[test]
fn a_tie_to_the_cent_goes_to_the_larger_amount() {
    let fee = || cost(CostKind::NetworkFee, SOL, "0.000005", 9, Charged::Extra);
    let ranked = rank(
        routes(
            (SOL, "0.1", 9),
            (SOL_USDC, 6),
            vec![
                Quote {
                    provider: "1Click",
                    method: Method::Deposit,
                    expected: "9.297282",
                    minimum: "9.204309",
                    costs: vec![fee()],
                },
                Quote {
                    provider: "Raydium",
                    method: Method::Pool,
                    expected: "9.340976",
                    minimum: "9.294271",
                    costs: vec![fee()],
                },
                Quote {
                    provider: "Relay",
                    method: Method::Aggregator,
                    expected: "9.324938",
                    minimum: "9.278313",
                    costs: vec![fee()],
                },
                Quote {
                    provider: "Jupiter",
                    method: Method::Aggregator,
                    expected: "9.342297",
                    minimum: "9.295586",
                    costs: vec![fee()],
                },
            ],
        ),
        prices(&[(SOL, "93.3"), (SOL_USDC, "1")]),
        2,
    );
    assert_eq!(
        providers(&ranked),
        ["Jupiter", "Raydium", "Relay", "1Click"]
    );
    assert_eq!(net(&ranked[0]), net(&ranked[1]));
}

#[test]
fn a_tie_to_the_cent_goes_to_the_route_where_nobody_holds_the_money() {
    let ranked = rank(
        routes(
            (SOL, "0.1", 9),
            (SOL_USDC, 6),
            vec![
                Quote {
                    provider: "deposit",
                    method: Method::Deposit,
                    expected: "9.3449",
                    minimum: "9.3",
                    costs: Vec::new(),
                },
                Quote {
                    provider: "pool",
                    method: Method::Pool,
                    expected: "9.3401",
                    minimum: "9.3",
                    costs: Vec::new(),
                },
            ],
        ),
        prices(&[(SOL, "93.3"), (SOL_USDC, "1")]),
        2,
    );
    assert_eq!(providers(&ranked), ["pool", "deposit"]);
}

#[test]
fn a_ton_pool_fee_inside_the_quote_is_shown_but_not_counted_twice() {
    let gas = || cost(CostKind::NetworkFee, GRAM, "0.045", 9, Charged::Extra);
    let ranked = rank(
        routes(
            (GRAM, "1", 9),
            (TON_USDT, 6),
            vec![
                Quote {
                    provider: "STON.fi",
                    method: Method::Pool,
                    expected: "1.167759",
                    minimum: "1.156081",
                    costs: vec![
                        gas(),
                        cost(CostKind::PoolFee, TON_USDT, "0.003518", 6, Charged::InQuote),
                    ],
                },
                Quote {
                    provider: "swap.coffee",
                    method: Method::Aggregator,
                    expected: "1.172069",
                    minimum: "1.160348",
                    costs: vec![gas()],
                },
            ],
        ),
        prices(&[(GRAM, "1.17"), (TON_USDT, "1")]),
        2,
    );
    assert_eq!(providers(&ranked), ["swap.coffee", "STON.fi"]);
    let stonfi = &ranked[1];
    assert_eq!(stonfi.value.as_ref().unwrap().extra.to_string(), "0.05");
    assert_eq!(net(stonfi), "1.12");
}

#[test]
fn routes_far_below_market_are_refused_and_unpriced_ones_go_after_priced_ones() {
    let mut quotes = routes(
        (ETH, "1", 18),
        (ETH_USDC, 6),
        vec![
            Quote {
                provider: "fair",
                method: Method::Pool,
                expected: "2240",
                minimum: "2230",
                costs: Vec::new(),
            },
            Quote {
                provider: "greedy",
                method: Method::Aggregator,
                expected: "2010",
                minimum: "2000",
                costs: Vec::new(),
            },
            Quote {
                provider: "broken",
                method: Method::Aggregator,
                expected: "2290",
                minimum: "2300",
                costs: Vec::new(),
            },
        ],
    );
    let mut unpriced = quotes[0].clone();
    unpriced.provider = "unpriced";
    unpriced.costs = vec![cost(
        CostKind::NetworkFee,
        BTC,
        "0.00001",
        8,
        Charged::Extra,
    )];
    quotes.push(unpriced);
    let ranked = rank(quotes, prices(&[(ETH, "2285"), (ETH_USDC, "1")]), 2);
    assert_eq!(providers(&ranked), ["fair", "unpriced", "greedy", "broken"]);
    assert!(ranked[0].refused.is_none());
    assert_eq!(ranked[1].cautions, [Caution::Unpriced]);
    assert_eq!(ranked[2].refused, Some(Refusal::BelowMarket(1203)));
    assert_eq!(ranked[3].refused, Some(Refusal::Inconsistent));
}

#[test]
fn a_rate_a_little_below_market_is_flagged_but_allowed() {
    let ranked = rank(
        routes(
            (ETH, "1", 18),
            (ETH_USDC, 6),
            vec![Quote {
                provider: "thin pool",
                method: Method::Pool,
                expected: "2200",
                minimum: "2190",
                costs: Vec::new(),
            }],
        ),
        prices(&[(ETH, "2285"), (ETH_USDC, "1")]),
        2,
    );
    assert!(ranked[0].refused.is_none());
    assert_eq!(ranked[0].cautions, [Caution::BelowMarket(371)]);
}
