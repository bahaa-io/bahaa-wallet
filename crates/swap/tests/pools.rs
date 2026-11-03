#![allow(clippy::unwrap_used)]

use std::time::Duration;

use bahaa_chain::{Chain, ContractSwap, PlannedCall, Step, TxId, TxStatus, check_call, now};
use bahaa_crypto::{Mnemonic, Seed, WordCount};
use bahaa_evm::{EvmCall, EvmChain};
use bahaa_swap::{Across, KyberSwap, Method, Order, Pools, Providers, Relay, Request, Velora};
use bahaa_testkit::anvil::Anvil;
use bahaa_types::{Address, Amount, AssetId};
use serde_json::json;

const USDT: &str = "0x55d398326f99059fF775485246999027B3197955";
const ROUTER: &str = "0x13f4EA83D0bd40E75C8222255bc855a974568Dd4";
const ACCOUNT: u32 = 0;

async fn forked() -> (Anvil, EvmChain, Address, Seed) {
    forked_on("bsc", "https://bsc-dataseed.bnbchain.org").await
}

async fn forked_on(network: &str, upstream: &str) -> (Anvil, EvmChain, Address, Seed) {
    let seed = Mnemonic::generate(WordCount::Twelve).unwrap().to_seed("");
    let anvil = Anvil::start(network, &["--fork-url", upstream]);
    anvil.ready().await;
    let chain = EvmChain::new(anvil.network()).unwrap();
    let me = chain.derive(&seed, ACCOUNT).unwrap();
    let code = anvil
        .rpc("eth_getCode", json!([me.to_string(), "latest"]))
        .await;
    assert_eq!(
        code,
        Some(json!("0x")),
        "the test account must be a plain account"
    );
    anvil
        .rpc(
            "anvil_setBalance",
            json!([me.to_string(), "0x56bc75e2d63100000"]),
        )
        .await
        .unwrap();
    (anvil, chain, me, seed)
}

fn request(sell: &str, amount: &str, buy: &str, me: Address) -> Request {
    Request {
        sell: sell.parse().unwrap(),
        amount: Amount::parse(amount, 18).unwrap(),
        buy: buy.parse().unwrap(),
        buy_decimals: 18,
        native_decimals: 18,
        from: me,
        to: me,
        deadline: now() + 1_800,
    }
}

async fn quoted(chain: &EvmChain, request: &Request) -> ContractSwap {
    let pools = Pools::new(&Providers::builtin().unwrap().pools).unwrap();
    let (route, order) = pools.quote(chain, request, now()).await.unwrap();
    assert_eq!(
        (route.provider, route.method),
        ("PancakeSwap", Method::Pool)
    );
    assert!(route.minimum < route.expected);
    assert_eq!(order.deposit.to_string(), ROUTER);
    swap_of(request, order)
}

async fn warm(anvil: &Anvil, swap: &ContractSwap) {
    let from = swap.from.to_string();
    let mut calls = Vec::new();
    if let (Some(token), Some(spender)) = (swap.sell.contract(), swap.spender) {
        calls.push(json!({
            "from": from,
            "to": token.to_string(),
            "data": format!(
                "0x095ea7b3{:0>64}{:064x}",
                spender.to_string().trim_start_matches("0x").to_lowercase(),
                swap.amount.units()
            ),
        }));
    }
    calls.push(json!({
        "from": from,
        "to": swap.call.to.to_string(),
        "value": format!("{:#x}", swap.call.value.units()),
        "data": format!("0x{}", bahaa_types::hex::encode(&swap.call.data)),
    }));
    let params = json!([{"blockStateCalls": [{"calls": calls}], "validation": false}, "latest"]);
    anvil.rpc("eth_simulateV1", params).await.unwrap();
}

fn swap_of(request: &Request, order: Order) -> ContractSwap {
    let contract = order.contract.unwrap();
    ContractSwap {
        account: ACCOUNT,
        from: request.from,
        call: contract.call,
        spender: contract.spender,
        sell: request.sell.clone(),
        amount: request.amount,
        buy: Some((request.buy.clone(), order.minimum)),
        ours: contract.ours,
        spent: contract.spent,
    }
}

async fn run(chain: &EvmChain, seed: &Seed, plans: &[EvmCall]) {
    for plan in plans {
        let signed = chain.sign_call(plan, seed).unwrap();
        check_call(plan.planned(), &chain.decode_call(&signed).unwrap()).unwrap();
        let id = chain.broadcast_private(&signed).await.unwrap();
        confirmed(chain, &id).await;
    }
}

async fn confirmed(chain: &EvmChain, id: &TxId) {
    for _ in 0..100 {
        match chain
            .status(id, &Address::Evm([0; 20]), None)
            .await
            .unwrap()
        {
            TxStatus::Confirmed { .. } => return,
            TxStatus::Failed(reason) => panic!("failed: {reason}"),
            _ => tokio::time::sleep(Duration::from_millis(100)).await,
        }
    }
    panic!("not confirmed");
}

async fn held(chain: &EvmChain, me: &Address, asset: &str) -> Amount {
    let asset: AssetId = asset.parse().unwrap();
    chain.balances(me, &[asset]).await.unwrap()[0].amount
}

#[tokio::test]
#[ignore = "needs a local anvil and internet: BAHAA_ANVIL=/path/to/anvil"]
async fn pancakeswap_swaps_bnb_for_usdt_and_back_with_an_exact_approval() {
    let (_anvil, chain, me, seed) = forked().await;
    let usdt = format!("bsc/{USDT}");
    let buying = quoted(&chain, &request("bsc", "1", &usdt, me)).await;
    let (plans, fee) = chain.prepare_swap(&buying).await.unwrap();
    assert_eq!(plans.len(), 1);
    assert_eq!(plans[0].planned().step, Step::Swap);
    assert!(fee.expected() <= fee.max());
    run(&chain, &seed, &plans).await;
    let bought = held(&chain, &me, &usdt).await;
    assert!(bought >= buying.buy.as_ref().unwrap().1);

    let before = held(&chain, &me, "bsc").await;
    let half = Amount::new(bought.units() / ruint::aliases::U256::from(2u8), 18).unwrap();
    let selling = quoted(&chain, &request(&usdt, &half.to_string(), "bsc", me)).await;
    let (plans, fee) = chain.prepare_swap(&selling).await.unwrap();
    let steps: Vec<&PlannedCall> = plans.iter().map(EvmCall::planned).collect();
    assert_eq!(steps.len(), 2);
    assert!(matches!(
        &steps[0].step,
        Step::Approve { amount, spender, .. } if *amount == half && spender.to_string() == ROUTER
    ));
    run(&chain, &seed, &plans).await;
    let after = held(&chain, &me, "bsc").await;
    let minimum = selling.buy.as_ref().unwrap().1;
    assert!(after.checked_add(fee.max()).unwrap() >= before.checked_add(minimum).unwrap());
    let left = held(&chain, &me, &usdt).await;
    assert_eq!(left, bought.checked_sub(half).unwrap());
    let again = chain.prepare_swap(&selling).await.unwrap().0;
    assert_eq!(
        again.len(),
        2,
        "the approval was used up, a new swap approves again"
    );
}

#[tokio::test]
#[ignore = "needs a local anvil and internet: BAHAA_ANVIL=/path/to/anvil"]
async fn a_swap_that_pays_less_or_someone_else_is_refused_before_signing() {
    let (_anvil, chain, me, _) = forked().await;
    let usdt = format!("bsc/{USDT}");
    let honest = quoted(&chain, &request("bsc", "1", &usdt, me)).await;

    let mut greedy = honest.clone();
    let (asset, minimum) = greedy.buy.take().unwrap();
    greedy.buy = Some((asset, minimum.checked_add(minimum).unwrap()));
    let refused = chain.prepare_swap(&greedy).await.unwrap_err();
    assert_eq!(
        refused.to_string(),
        "the swap would pay less than its minimum"
    );

    let mut elsewhere = honest.clone();
    let Address::Evm(mine) = me else { panic!() };
    let start = elsewhere
        .call
        .data
        .windows(20)
        .position(|window| window == mine)
        .unwrap();
    elsewhere.call.data[start..start + 20].copy_from_slice(&[0x77; 20]);
    let refused = chain.prepare_swap(&elsewhere).await.unwrap_err();
    assert_eq!(
        refused.to_string(),
        "the swap would pay less than its minimum"
    );

    let mut cheaper = honest;
    cheaper.amount = Amount::parse("0.5", 18).unwrap();
    assert!(chain.prepare_swap(&cheaper).await.is_err());
}

#[tokio::test]
#[ignore = "needs a local anvil and internet: BAHAA_ANVIL=/path/to/anvil"]
async fn kyberswap_routes_pass_the_check_and_settle_on_a_forked_chain() {
    let (anvil, chain, me, seed) = forked().await;
    let usdt = format!("bsc/{USDT}");
    let kyber = KyberSwap::new(
        &Providers::builtin().unwrap().kyberswap,
        bahaa_chain::http::client().unwrap(),
    )
    .unwrap();
    let buy = request("bsc", "1", &usdt, me);
    let (route, order) = kyber.quote(&chain, &buy, now()).await.unwrap();
    assert_eq!(
        (route.provider, route.method),
        ("KyberSwap", Method::Aggregator)
    );
    let buying = swap_of(&buy, order);
    warm(&anvil, &buying).await;
    let (plans, _) = chain.prepare_swap(&buying).await.unwrap();
    assert_eq!(plans.len(), 1);
    assert!(plans[0].checked());
    run(&chain, &seed, &plans).await;
    let bought = held(&chain, &me, &usdt).await;
    assert!(bought >= buying.buy.as_ref().unwrap().1);

    let half = Amount::new(bought.units() / ruint::aliases::U256::from(2u8), 18).unwrap();
    let sell = request(&usdt, &half.to_string(), "bsc", me);
    let (_, order) = kyber.quote(&chain, &sell, now()).await.unwrap();
    let selling = swap_of(&sell, order);
    warm(&anvil, &selling).await;
    let (plans, _) = chain.prepare_swap(&selling).await.unwrap();
    assert_eq!(plans.len(), 2);
    run(&chain, &seed, &plans).await;
    assert_eq!(
        held(&chain, &me, &usdt).await,
        bought.checked_sub(half).unwrap()
    );
}

#[tokio::test]
#[ignore = "needs a local anvil and internet: BAHAA_ANVIL=/path/to/anvil"]
async fn an_across_deposit_takes_exactly_the_amount_on_a_forked_chain() {
    let (anvil, chain, me, seed) = forked_on("base", "https://mainnet.base.org").await;
    let usdc = "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913";
    let Address::Evm(owner) = me else { panic!() };
    anvil
        .fund_token(usdc, &owner, &format!("{:x}", 500_000_000u64))
        .await;
    let across = Across::new(
        &Providers::builtin().unwrap().across,
        bahaa_chain::http::client().unwrap(),
    )
    .unwrap();
    let deposit = Request {
        sell: format!("base/{usdc}").parse().unwrap(),
        amount: Amount::parse("100", 6).unwrap(),
        buy: "arbitrum/0xaf88d065e77c8cC2239327C5EDb3A432268e5831"
            .parse()
            .unwrap(),
        buy_decimals: 6,
        native_decimals: 18,
        from: me,
        to: me,
        deadline: now() + 1_800,
    };
    let (route, order) = across.quote(&chain, &deposit, now()).await.unwrap();
    assert_eq!(route.method, Method::Escrow);
    let mut swap = swap_of(&deposit, order);
    swap.buy = None;
    warm(&anvil, &swap).await;
    let (plans, _) = chain.prepare_swap(&swap).await.unwrap();
    assert_eq!(plans.len(), 2);
    assert!(plans.iter().all(EvmCall::checked));
    run(&chain, &seed, &plans).await;
    assert_eq!(
        held(&chain, &me, &format!("base/{usdc}")).await,
        Amount::parse("400", 6).unwrap()
    );
}

#[tokio::test]
#[ignore = "needs a local anvil and internet: BAHAA_ANVIL=/path/to/anvil"]
async fn velora_routes_pass_the_check_and_pay_after_their_fee_on_a_forked_chain() {
    let (anvil, chain, me, seed) = forked().await;
    let usdt = format!("bsc/{USDT}");
    let velora = Velora::new(
        &Providers::builtin().unwrap().velora,
        bahaa_chain::http::client().unwrap(),
    )
    .unwrap();
    let buy = request("bsc", "1", &usdt, me);
    let (route, order) = velora.quote(&chain, &buy, now()).await.unwrap();
    assert_eq!(
        (route.provider, route.method),
        ("Velora", Method::Aggregator)
    );
    let buying = swap_of(&buy, order);
    warm(&anvil, &buying).await;
    let (plans, _) = chain.prepare_swap(&buying).await.unwrap();
    run(&chain, &seed, &plans).await;
    let bought = held(&chain, &me, &usdt).await;
    assert!(bought >= route.minimum);

    let half = Amount::new(bought.units() / ruint::aliases::U256::from(2u8), 18).unwrap();
    let sell = request(&usdt, &half.to_string(), "bsc", me);
    let (_, order) = velora.quote(&chain, &sell, now()).await.unwrap();
    let selling = swap_of(&sell, order);
    warm(&anvil, &selling).await;
    let (plans, _) = chain.prepare_swap(&selling).await.unwrap();
    assert_eq!(plans.len(), 2);
    run(&chain, &seed, &plans).await;
    assert_eq!(
        held(&chain, &me, &usdt).await,
        bought.checked_sub(half).unwrap()
    );
}

#[tokio::test]
#[ignore = "needs a local anvil and internet: BAHAA_ANVIL=/path/to/anvil"]
async fn a_relay_call_from_polygon_takes_exactly_the_amount_on_a_forked_chain() {
    let (anvil, chain, me, seed) = forked_on("polygon", "https://polygon.drpc.org").await;
    let usdt = "0xc2132D05D31c914a87C6611C10748AEb04B58e8F";
    let Address::Evm(owner) = me else { panic!() };
    anvil
        .fund_token(usdt, &owner, &format!("{:x}", 50_000_000u64))
        .await;
    let relay = Relay::new(
        &Providers::builtin().unwrap().relay,
        bahaa_chain::http::client().unwrap(),
    )
    .unwrap();
    let deposit = Request {
        sell: format!("polygon/{usdt}").parse().unwrap(),
        amount: Amount::parse("20", 6).unwrap(),
        buy: "base/0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913"
            .parse()
            .unwrap(),
        buy_decimals: 6,
        native_decimals: 18,
        from: me,
        to: me,
        deadline: now() + 1_800,
    };
    let (_, order) = relay.quote(&deposit, now()).await.unwrap();
    let mut swap = swap_of(&deposit, order);
    swap.buy = None;
    warm(&anvil, &swap).await;
    let (plans, _) = chain.prepare_swap(&swap).await.unwrap();
    assert_eq!(plans.len(), 2);
    run(&chain, &seed, &plans).await;
    assert_eq!(
        held(&chain, &me, &format!("polygon/{usdt}")).await,
        Amount::parse("30", 6).unwrap()
    );
}
