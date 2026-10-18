#![allow(clippy::unwrap_used)]

use bahaa_chain::{Chain, ChainError, Signed, TxId, TxStatus};
use bahaa_config::{Protocol, Registry};
use bahaa_ton::TonChain;
use bahaa_types::{Address, Amount, AssetId, ChainFamily};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use ruint::aliases::U256;

const USDT: &str = "EQCxE6mUtQJKFnGfaROTKOt1lZbDiiX1kCixRv7Nw2Id_sDs";
const HOLDER: &str = "0:f8b8635d0786b77f9da660e65fd31d670a675dd4302facba692f46b571eb5360";
const SENDER: &str = "0:13fd9907ed89660d9b90f925bd54a6b029cafb368a172888f1ffc6fd690624e5";
const SENT: &str = "b5ee9c72410205010001100001e5880027fb320fdb12cc1b3721f24b7aa94d605395f66d142e5111e3ff8dfad20c49ca039b4b3b73fffff88b566b9008000000559e4f7c61649cead47b81fc559961f1397ad6d17bf54953722f26dc0e635781c76df6d6f81c372949e1c8cb6a2cd85973bdab865f605980a59459cdf5e17aca0701020a0ec3c86d0302030000016862005bfa00866642a075cac79e914142651bcacae9fd297238395e0f73c0e9af00f3a018685d68000000000000000000000000010400ac0f8a7ea5000000006acd70d7506cc74a400801bca4e3e99d4b7eeed43fa097305639a10a68520be36b0b239c7ece42e7159acf0004ff6641fb62598366e43e496f5529ac0a72becda285ca223c7ff1bf5a41893942027a92ffc8";
const SENT_ID: &str = "88d7201d27158e7cbaf4cceaef8ce46f460e9dc1e084490b2485eb6e6112bd54";
const SENT_LT: u64 = 109_578_663_000_001;
const SENT_EXPIRES: u64 = 1_791_848_961;

fn unhex(text: &str) -> Vec<u8> {
    (0..text.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(&text[at..at + 2], 16).unwrap())
        .collect()
}

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

#[tokio::test]
#[ignore = "talks to the real TON network, run with --test-threads=1"]
async fn live_v5r1_quote_follows_the_network_fee_settings() {
    let registry = Registry::builtin().unwrap();
    let ton = registry.get(&"ton".parse().unwrap()).unwrap().clone();
    let config: serde_json::Value = reqwest::get("https://tonapi.io/v2/blockchain/config")
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let gas = &config["21"]["gas_limits_prices"];
    let field = |value: &serde_json::Value| value.as_u64().unwrap();
    let extra = (4_939 - field(&gas["flat_gas_limit"])) * field(&gas["gas_price"]);
    let compute = field(&gas["flat_gas_price"]) + extra.div_ceil(65_536);
    let lump = field(&config["25"]["msg_forward_prices"]["lump_price"]);
    let transfer = bahaa_chain::Transfer {
        account: 0,
        from: Address::parse(
            ChainFamily::Ton,
            "0:13fd9907ed89660d9b90f925bd54a6b029cafb368a172888f1ffc6fd690624e5",
        )
        .unwrap(),
        to: Address::Ton {
            workchain: 0,
            hash: [0x5c; 32],
        },
        asset: "ton".parse().unwrap(),
        amount: bahaa_types::Amount::parse("0.01", 9).unwrap(),
    };
    let nano = |text: &str| bahaa_types::Amount::parse(text, 9).unwrap();
    let mut seen = Vec::new();
    for protocol in [Protocol::TonLiteservers, Protocol::TonApi] {
        let mut network = ton.clone();
        network
            .endpoints
            .retain(|endpoint| endpoint.protocol == protocol);
        let chain = TonChain::new(network).unwrap();
        let (_, quote) = chain.prepare(&transfer).await.unwrap();
        let lines = quote.lines();
        assert_eq!(lines[0].item, bahaa_chain::FeeItem::Compute { gas: 4_939 });
        assert_eq!(
            lines[0].amount.units(),
            ruint::aliases::U256::from(compute),
            "{protocol:?}"
        );
        assert!(lines[1].amount < nano("0.0001"), "{protocol:?}: {quote:?}");
        assert!(
            lines[2].amount.units() > ruint::aliases::U256::from(2 * lump),
            "{protocol:?}"
        );
        assert!(
            quote.expected() > nano("0.0004") && quote.expected() < nano("0.0007"),
            "{quote:?}"
        );
        assert_eq!(
            quote.max().units(),
            quote.expected().units() * ruint::aliases::U256::from(2u8)
        );
        seen.push((lines[0].amount, lines[2].amount));
        tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
    }
    assert_eq!(seen[0], seen[1]);
    let mut center = ton.clone();
    center
        .endpoints
        .retain(|endpoint| endpoint.protocol == Protocol::TonCenter);
    assert!(
        TonChain::new(center)
            .unwrap()
            .prepare(&transfer)
            .await
            .is_err()
    );
}

#[tokio::test]
#[ignore = "talks to the real TON network, run with --test-threads=1"]
async fn live_a_real_transfer_is_tracked_and_its_message_cannot_be_sent_again() {
    let registry = Registry::builtin().unwrap();
    let ton = registry.get(&"ton".parse().unwrap()).unwrap().clone();
    let explorer: serde_json::Value = reqwest::get(format!(
        "https://toncenter.com/api/v3/transactions?account={SENDER}&lt={SENT_LT}&limit=1"
    ))
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    let indexed = &explorer["transactions"][0];
    assert_eq!(
        indexed["in_msg"]["hash_norm"],
        STANDARD.encode(unhex(SENT_ID))
    );
    let nano = |value: &serde_json::Value| value.as_str().unwrap().parse::<u64>().unwrap();
    let paid = nano(&indexed["total_fees"]) + nano(&indexed["out_msgs"][0]["fwd_fee"]);
    let confirmed = TxStatus::Confirmed {
        block: None,
        fee: Some(Amount::new(U256::from(paid), 9).unwrap()),
    };
    let sender = Address::parse(ChainFamily::Ton, SENDER).unwrap();
    let signed = Signed {
        id: TxId::new(SENT_ID.to_owned()).unwrap(),
        bytes: unhex(SENT),
    };
    let only = |protocol: Protocol| {
        let mut network = ton.clone();
        network
            .endpoints
            .retain(|endpoint| endpoint.protocol == protocol);
        TonChain::new(network).unwrap()
    };
    for protocol in [Protocol::TonLiteservers, Protocol::TonApi] {
        tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
        let status = only(protocol)
            .status(&signed.id, &sender, Some(SENT_EXPIRES))
            .await;
        assert_eq!(status, Ok(confirmed.clone()), "{protocol:?}");
    }
    for protocol in [
        Protocol::TonLiteservers,
        Protocol::TonApi,
        Protocol::TonCenter,
    ] {
        tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
        assert_eq!(
            only(protocol).broadcast(&signed).await,
            Err(ChainError::Rejected(
                "the wallet contract refused the message with exit code 133".to_owned()
            )),
            "{protocol:?}"
        );
    }
}
