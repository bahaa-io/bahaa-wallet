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

#[tokio::test]
#[ignore = "talks to the real BNB Chain, Arbitrum, Polygon and Ethereum"]
async fn live_space_id_and_unstoppable_names_resolve_only_while_held() {
    let registry = Registry::builtin().unwrap();
    let chain =
        |id: &str| EvmChain::new(registry.get(&id.parse().unwrap()).unwrap().clone()).unwrap();
    let hex = |bytes: [u8; 20]| bahaa_types::hex::encode(&bytes);
    let spaceid = Some("b5932a6b7d50a966aec6c74c97385412fb497540".to_owned());
    assert_eq!(
        chain("bsc")
            .space_id_address("spaceid.bnb")
            .await
            .unwrap()
            .map(hex),
        spaceid
    );
    assert_eq!(
        chain("arbitrum")
            .space_id_address("spaceid.arb")
            .await
            .unwrap()
            .map(hex),
        spaceid
    );
    assert_eq!(
        chain("arbitrum")
            .space_id_address("doge.arb")
            .await
            .unwrap(),
        None
    );
    let polygon = chain("polygon");
    assert_eq!(
        polygon
            .unstoppable_address("brad.crypto", "BTC", "BTC")
            .await
            .unwrap()
            .as_deref(),
        Some("bc1q359khn0phg58xgezyqsuuaha28zkwx047c0c3y")
    );
    assert_eq!(
        polygon
            .unstoppable_address("zz-nothing-4417.crypto", "ETH", "ETH")
            .await
            .unwrap(),
        None
    );
}
