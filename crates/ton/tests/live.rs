#![allow(clippy::unwrap_used)]

use bahaa_config::{Protocol, Registry};
use bahaa_ton::TonChain;
use bahaa_types::{Address, AssetId, ChainFamily};

const USDT: &str = "EQCxE6mUtQJKFnGfaROTKOt1lZbDiiX1kCixRv7Nw2Id_sDs";
const HOLDER: &str = "0:f8b8635d0786b77f9da660e65fd31d670a675dd4302facba692f46b571eb5360";

#[tokio::test]
#[ignore = "talks to the real TON network"]
async fn live_gram_and_usdt_through_each_provider() {
    let registry = Registry::builtin().unwrap();
    let ton = registry.get(&"ton".parse().unwrap()).unwrap().clone();
    let holder = Address::parse(ChainFamily::Ton, HOLDER).unwrap();
    let assets: Vec<AssetId> = ["ton".to_owned(), format!("ton/{USDT}")]
        .iter()
        .map(|asset| asset.parse().unwrap())
        .collect();
    let mut seen = Vec::new();
    for protocol in [Some(Protocol::TonCenter), Some(Protocol::TonApi), None] {
        let mut network = ton.clone();
        if let Some(protocol) = protocol {
            network
                .endpoints
                .retain(|endpoint| endpoint.protocol == protocol);
        }
        let chain = TonChain::new(network).unwrap();
        let balances = chain.balances(&holder, &assets).await.unwrap();
        assert!(!balances[0].amount.is_zero(), "{protocol:?}: {balances:?}");
        assert!(!balances[1].amount.is_zero(), "{protocol:?}: {balances:?}");
        assert_eq!(balances[1].amount.decimals(), 6);
        seen.push(balances[1].amount);
        tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
    }
    assert!(seen.windows(2).all(|pair| pair[0] == pair[1]), "{seen:?}");
}
