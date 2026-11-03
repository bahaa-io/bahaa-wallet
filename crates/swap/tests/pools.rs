#![allow(clippy::unwrap_used)]

use std::time::Duration;

use bahaa_chain::{Chain, ContractSwap, PlannedCall, Step, TxId, TxStatus, check_call, now};
use bahaa_crypto::{Mnemonic, Seed, WordCount};
use bahaa_evm::{EvmCall, EvmChain};
use bahaa_swap::{Method, Pools, Providers, Request};
use bahaa_testkit::anvil::Anvil;
use bahaa_types::{Address, Amount, AssetId};
use serde_json::json;

const USDT: &str = "0x55d398326f99059fF775485246999027B3197955";
const ROUTER: &str = "0x13f4EA83D0bd40E75C8222255bc855a974568Dd4";
const ACCOUNT: u32 = 0;

async fn forked() -> (Anvil, EvmChain, Address, Seed) {
    let seed = Mnemonic::generate(WordCount::Twelve).unwrap().to_seed("");
    let anvil = Anvil::start("bsc", &["--fork-url", "https://bsc-dataseed.bnbchain.org"]);
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
    let contract = order.contract.unwrap();
    assert_eq!(contract.call.to.to_string(), ROUTER);
    ContractSwap {
        account: ACCOUNT,
        from: request.from,
        call: contract.call,
        spender: contract.spender,
        sell: request.sell.clone(),
        amount: request.amount,
        buy: Some((request.buy.clone(), order.minimum)),
        ours: contract.ours,
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
