#![allow(clippy::unwrap_used)]

use bahaa_config::Registry;
use bahaa_evm::EvmChain;

#[tokio::test]
#[ignore = "talks to the real Ethereum mainnet and a Basename gateway"]
async fn live_ens_names_resolve_on_chain_and_off_chain() {
    let registry = Registry::builtin().unwrap();
    let chain = EvmChain::new(registry.get(&"ethereum".parse().unwrap()).unwrap().clone()).unwrap();
    let hex = |bytes: Vec<u8>| bahaa_types::hex::encode(&bytes);
    assert_eq!(
        chain.ens_address("vitalik.eth", 60).await.unwrap().map(hex),
        Some("d8da6bf26964af9d7eed9e03e53415d37aa96045".to_owned())
    );
    assert_eq!(
        chain
            .ens_address("jesse.base.eth", 0x8000_0000 | 0x2105)
            .await
            .unwrap()
            .map(hex),
        Some("2211d1d0020daea8039e46cf1367962070d77da9".to_owned())
    );
    assert_eq!(
        chain
            .ens_address("this-name-should-not-exist-4417.eth", 60)
            .await
            .unwrap(),
        None
    );
}
