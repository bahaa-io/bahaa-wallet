#![allow(clippy::unwrap_used)]

mod common;

use bahaa_chain::{Chain, FeeItem, Transfer};
use bahaa_config::Registry;
use bahaa_solana::SolanaChain;
use bahaa_types::Amount;
use common::{FRIEND, address, native, usdc};

const MAINNET: &str = r##"[{
    "id": "solana",
    "name": "Solana",
    "color": "#9945ff",
    "family": "solana",
    "genesis": "45296998a6f8e2a784db5d9f95e18fc23f70441a1039446801089879b08c7ef0",
    "testnet": false,
    "native": {"symbol": "SOL", "name": "Solana", "decimals": 9},
    "tokens": [{"contract": "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v", "symbol": "USDC", "name": "USD Coin", "decimals": 6}],
    "endpoints": [
        {"protocol": "solana-rpc", "url": "https://api.mainnet-beta.solana.com", "features": ["history"]},
        {"protocol": "solana-rpc", "url": "https://solana-rpc.publicnode.com"}
    ],
    "explorer": {"tx": "https://solscan.io/tx/{tx}", "address": "https://solscan.io/account/{address}"}
}]"##;

const RICH: &str = "9WzDXwBbmkg8ZTbNMqUxvQRAyrZzDsGYdLVL9zYtAWWM";

fn mainnet() -> SolanaChain {
    let registry = Registry::from_json(MAINNET).unwrap();
    SolanaChain::new(registry.get(&"solana".parse().unwrap()).unwrap().clone()).unwrap()
}

#[tokio::test]
#[ignore = "talks to the real Solana mainnet"]
async fn live_balances_and_a_quote_from_a_funded_address() {
    let chain = mainnet();
    let balances = chain
        .balances(&address(RICH), &[native(), usdc()])
        .await
        .unwrap();
    assert!(!balances[0].amount.is_zero());
    let transfer = Transfer {
        account: 0,
        from: address(RICH),
        to: address(FRIEND),
        asset: native(),
        amount: Amount::parse("0.001", 9).unwrap(),
        max: false,
        memo: Some("live quote".to_owned()),
    };
    let (plan, fee) = chain.prepare(&transfer).await.unwrap();
    assert_eq!(chain.planned(&plan).amount, transfer.amount);
    assert_eq!(fee.lines()[0].item, FeeItem::Signature);
    assert!(matches!(fee.lines()[1].item, FeeItem::Priority { units } if units > 0));
}
