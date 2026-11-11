#![allow(clippy::unwrap_used)]

use bahaa_chain::{Fetcher, Gateways};

#[tokio::test]
#[ignore = "reads real details files from IPFS, Arweave and Fragment"]
async fn live_details_files_are_read_through_the_gateways() {
    let fetcher = Fetcher::new(Gateways::builtin().unwrap()).unwrap();
    for link in [
        "ipfs://QmeSjSinHpPnmXmspMjwiXyN6zS4E9zccariGR3jxcaWtq/1",
        "https://ipfs.io/ipfs/QmeSjSinHpPnmXmspMjwiXyN6zS4E9zccariGR3jxcaWtq/1",
    ] {
        let ape = fetcher.details(link).await.unwrap();
        assert_eq!(
            ape.image.as_deref(),
            Some("ipfs://QmPbxeGcXhYQQNgsC6a36dDyYUcHgMLnGKnF8pVFmGsvqi"),
            "{link}"
        );
    }
    let gnome = fetcher
        .details("https://arweave.net/fi67iYDS-jWtdKfRu0aFxu_nRjKZnHTMo-9SEKBI17I")
        .await
        .unwrap();
    assert_eq!(gnome.name.as_deref(), Some("Pawnshop gnomie #3859"));
    assert_eq!(gnome.about.as_deref(), Some("DAO-based NFT pawnshop"));
    let username = fetcher
        .details("https://nft.fragment.com/username/r6rrrrrrrrr.json")
        .await
        .unwrap();
    assert_eq!(username.name.as_deref(), Some("@r6rrrrrrrrr"));
    assert_eq!(
        username.image.as_deref(),
        Some("https://nft.fragment.com/username/r6rrrrrrrrr.webp")
    );
}
