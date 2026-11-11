#![allow(clippy::unwrap_used)]

use bahaa_chain::{Chain, Transfer};
use bahaa_config::Registry;
use bahaa_evm::EvmChain;
use bahaa_types::{Address, Amount, ChainFamily, NftKind};

const COLLECTOR: &str = "0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045";

#[tokio::test]
#[ignore = "talks to the real Ethereum, Optimism and Avalanche networks"]
async fn live_listed_nfts_are_proven_by_our_own_nodes() {
    let registry = Registry::builtin().unwrap();
    let owner = Address::parse(ChainFamily::Evm, COLLECTOR).unwrap();
    for id in ["ethereum", "optimism", "avalanche"] {
        let chain = EvmChain::new(registry.get(&id.parse().unwrap()).unwrap().clone()).unwrap();
        let found = chain.nfts(&owner).await.unwrap();
        assert!(!found.items.is_empty(), "{id}: nothing proven");
        for item in &found.items {
            assert!(
                matches!(item.kind, NftKind::Erc721 | NftKind::Erc1155),
                "{id}: {item:?}"
            );
            assert!(
                item.collection.as_ref().unwrap().address.is_some(),
                "{id}: {item:?}"
            );
            assert_eq!(item.asset.network().as_str(), id);
        }
        for kind in [NftKind::Erc721, NftKind::Erc1155] {
            let Some(item) = found.items.iter().find(|item| item.kind == kind) else {
                continue;
            };
            let transfer = Transfer {
                account: 0,
                from: owner,
                to: Address::Evm([0x5c; 20]),
                asset: item.asset.clone(),
                amount: Amount::parse("1", 0).unwrap(),
                max: false,
                memo: None,
            };
            let (plan, fee) = chain.prepare(&transfer).await.unwrap();
            assert_eq!(chain.planned(&plan).asset, item.asset, "{id}");
            assert!(!fee.expected().is_zero(), "{id}");
        }
    }
}
