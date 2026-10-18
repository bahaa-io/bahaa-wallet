#![allow(clippy::unwrap_used)]

use bahaa_config::{Protocol, Registry};
use bahaa_ton::TonChain;
use bahaa_types::{Address, AssetId, ChainFamily};

const USDT: &str = "EQCxE6mUtQJKFnGfaROTKOt1lZbDiiX1kCixRv7Nw2Id_sDs";
const HOLDER: &str = "0:f8b8635d0786b77f9da660e65fd31d670a675dd4302facba692f46b571eb5360";

#[tokio::test]
#[ignore = "talks to the real TON network, run with --test-threads=1"]
async fn live_gram_and_usdt_through_each_provider() {
    let registry = Registry::builtin().unwrap();
    let ton = registry.get(&"ton".parse().unwrap()).unwrap().clone();
    let holder = Address::parse(ChainFamily::Ton, HOLDER).unwrap();
    let assets: Vec<AssetId> = ["ton".to_owned(), format!("ton/{USDT}")]
        .iter()
        .map(|asset| asset.parse().unwrap())
        .collect();
    let mut seen = Vec::new();
    for protocol in [
        Some(Protocol::TonLiteservers),
        Some(Protocol::TonCenter),
        Some(Protocol::TonApi),
        None,
    ] {
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

#[tokio::test]
#[ignore = "talks to the real TON network, run with --test-threads=1"]
async fn live_fresh_accounts_hold_nothing_on_every_source() {
    let registry = Registry::builtin().unwrap();
    let ton = registry.get(&"ton".parse().unwrap()).unwrap().clone();
    let fresh = Address::Ton {
        workchain: 0,
        hash: [
            0x5c, 0x9d, 0x2f, 0x41, 0x66, 0x13, 0x37, 0xa8, 0x0e, 0x71, 0xc4, 0x58, 0x2b, 0x9a,
            0xf0, 0x06, 0xd3, 0x4e, 0x81, 0x27, 0x5c, 0x9d, 0x2f, 0x41, 0x66, 0x13, 0x37, 0xa8,
            0x0e, 0x71, 0xc4, 0x58,
        ],
    };
    let assets: Vec<AssetId> = ["ton".to_owned(), format!("ton/{USDT}")]
        .iter()
        .map(|asset| asset.parse().unwrap())
        .collect();
    for protocol in [
        Protocol::TonLiteservers,
        Protocol::TonCenter,
        Protocol::TonApi,
    ] {
        let mut network = ton.clone();
        network
            .endpoints
            .retain(|endpoint| endpoint.protocol == protocol);
        let chain = TonChain::new(network).unwrap();
        let balances = chain.balances(&fresh, &assets).await.unwrap();
        assert!(
            balances.iter().all(|balance| balance.amount.is_zero()),
            "{protocol:?}: {balances:?}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
    }
}
