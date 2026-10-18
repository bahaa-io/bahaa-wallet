#![allow(clippy::unwrap_used)]

use std::fmt::Write;

use bahaa_chain::{Chain, ChainError, ScanCursor, Signed, TxId, TxStatus};
use bahaa_config::{Protocol, Registry};
use bahaa_ton::TonChain;
use bahaa_types::{Address, Amount, AssetId, ChainFamily};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use ruint::aliases::U256;

const USDT: &str = "EQCxE6mUtQJKFnGfaROTKOt1lZbDiiX1kCixRv7Nw2Id_sDs";
const HOLDER: &str = "0:f8b8635d0786b77f9da660e65fd31d670a675dd4302facba692f46b571eb5360";
const SENDER: &str = "0:13fd9907ed89660d9b90f925bd54a6b029cafb368a172888f1ffc6fd690624e5";
const USDT_RAW: &str = "0:b113a994b5024a16719f69139328eb759596c38a25f59028b146fecdc3621dfe";
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
    let only = |protocol: Option<Protocol>| {
        let mut network = ton.clone();
        if let Some(protocol) = protocol {
            network
                .endpoints
                .retain(|endpoint| endpoint.protocol == protocol);
        }
        TonChain::new(network).unwrap()
    };
    let mut seen = Vec::new();
    for protocol in [Some(Protocol::TonLiteservers), Some(Protocol::TonApi), None] {
        let balances = only(protocol).balances(&holder, &assets).await.unwrap();
        assert!(!balances[0].amount.is_zero(), "{protocol:?}: {balances:?}");
        assert!(!balances[1].amount.is_zero(), "{protocol:?}: {balances:?}");
        assert_eq!(balances[1].amount.decimals(), 6);
        seen.push(balances[1].amount);
        tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
    }
    assert!(seen.windows(2).all(|pair| pair[0] == pair[1]), "{seen:?}");
    let center = only(Some(Protocol::TonCenter));
    assert!(
        !center.balances(&holder, &assets[..1]).await.unwrap()[0]
            .amount
            .is_zero()
    );
    assert!(center.balances(&holder, &assets).await.is_err());
    let remote: serde_json::Value = reqwest::get(
        "https://tonapi.io/v2/blockchain/accounts/0:7f024f7ec0cc57b87eaf017046adaf9f6c521afb809a159cb2a2194f4153356f/methods/get_wallet_data",
    )
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    let units = remote["decoded"]["balance"].as_str().unwrap();
    assert_eq!(seen[0].units().to_string(), units);
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
        let readable = if protocol == Protocol::TonCenter {
            assert!(chain.balances(&fresh, &assets).await.is_err());
            &assets[..1]
        } else {
            &assets[..]
        };
        let balances = chain.balances(&fresh, readable).await.unwrap();
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
    let sent = &indexed["out_msgs"][0];
    assert_eq!(sent["opcode"], "0x0f8a7ea5");
    tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
    let refund = indexed_transaction(SENDER, 109_578_663_000_008).await;
    let paid = nano(&indexed["total_fees"]) + nano(&sent["fwd_fee"]) + nano(&sent["value"])
        - nano(&refund["in_msg"]["value"])
        + nano(&refund["total_fees"]);
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
    let named = "the wallet contract refused the message with exit code 133";
    let mut reasons = Vec::new();
    for protocol in [
        Protocol::TonLiteservers,
        Protocol::TonApi,
        Protocol::TonCenter,
    ] {
        tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
        let Err(ChainError::Rejected(reason)) = only(protocol).broadcast(&signed).await else {
            panic!("{protocol:?} took a message the wallet already used");
        };
        assert!(
            reason == named || reason == "the network refused the message",
            "{protocol:?}: {reason}"
        );
        reasons.push(reason);
    }
    assert!(reasons.iter().any(|reason| reason == named), "{reasons:?}");
}

async fn indexed_receipts(account: &str, since: u64) -> Vec<(String, u64)> {
    let listed: serde_json::Value = reqwest::get(format!(
        "https://toncenter.com/api/v3/transactions?account={account}&start_utime={since}&limit=100&sort=desc"
    ))
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    let mut receipts = Vec::new();
    for transaction in listed["transactions"].as_array().unwrap() {
        let message = &transaction["in_msg"];
        let plain = [
            serde_json::Value::Null,
            "0x00000000".into(),
            "0x2167da4b".into(),
        ]
        .contains(&message["opcode"]);
        let kept = message["source"].is_string()
            && message["bounced"] == false
            && message["init_state"].is_null()
            && transaction["description"].get("bounce").is_none();
        let nano = message["value"]
            .as_str()
            .map_or(0, |value| value.parse::<u64>().unwrap());
        if plain && kept && nano > 0 {
            let hash = STANDARD
                .decode(transaction["hash"].as_str().unwrap())
                .unwrap();
            let hex = hash.iter().fold(String::new(), |mut out, byte| {
                let _ = write!(out, "{byte:02x}");
                out
            });
            receipts.push((hex, nano));
        }
    }
    receipts
}

async fn indexed_tokens(owner: &str, since: u64) -> Vec<(u64, u64, String)> {
    let listed: serde_json::Value = reqwest::get(format!(
        "https://toncenter.com/api/v3/jetton/transfers?owner_address={owner}&jetton_master={USDT_RAW}&direction=in&start_utime={since}&limit=100&sort=desc"
    ))
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    listed["jetton_transfers"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|transfer| transfer["transaction_aborted"] == false)
        .map(|transfer| {
            (
                transfer["transaction_now"].as_u64().unwrap(),
                transfer["amount"].as_str().unwrap().parse().unwrap(),
                transfer["source"].as_str().unwrap().to_ascii_lowercase(),
            )
        })
        .filter(|(_, units, _)| *units > 0)
        .collect()
}

#[tokio::test]
#[ignore = "talks to the real TON network, run with --test-threads=1"]
async fn live_incoming_gram_and_usdt_match_the_indexer() {
    let registry = Registry::builtin().unwrap();
    let ton = registry.get(&"ton".parse().unwrap()).unwrap().clone();
    let gram: AssetId = "ton".parse().unwrap();
    let assets = [gram.clone(), format!("ton/{USDT}").parse().unwrap()];
    let busy = Address::parse(ChainFamily::Ton, SENDER).unwrap();
    let since = ScanCursor {
        next: 1_791_848_664,
        complete_from: 1_791_848_664,
        complete_since: 1_791_848_664,
    };
    let test_seed = Address::parse(
        ChainFamily::Ton,
        "0:c73162b34cf19667210af3180d6fcfb88df1678cbbbe731f38642534cc6951e4",
    )
    .unwrap();
    let mut usdt_seen = 0;
    for (protocol, owner, cursor) in [
        (Protocol::TonLiteservers, busy, Some(since)),
        (Protocol::TonLiteservers, test_seed, None),
        (Protocol::TonApi, busy, Some(since)),
    ] {
        let mut network = ton.clone();
        network
            .endpoints
            .retain(|endpoint| endpoint.protocol == protocol);
        let chain = TonChain::new(network).unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
        let scanned = chain.scan(&owner, &assets, cursor).await.unwrap().unwrap();
        let window = cursor.map_or(scanned.cursor.complete_since, |cursor| cursor.next);
        let recent = scanned
            .found
            .iter()
            .filter(|incoming| incoming.time >= window);
        let (grams, tokens): (Vec<_>, Vec<_>) = recent.partition(|incoming| incoming.asset == gram);
        let found: Vec<(String, u64)> = grams
            .iter()
            .map(|incoming| {
                (
                    incoming.id.as_str().to_owned(),
                    u64::try_from(incoming.amount.units()).unwrap(),
                )
            })
            .collect();
        tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
        let indexed = indexed_receipts(&owner.to_string(), window).await;
        assert_eq!(found, indexed, "{protocol:?} {owner}");
        tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
        let indexed = indexed_tokens(&owner.to_string(), window).await;
        assert_eq!(tokens.len(), indexed.len(), "{protocol:?} {owner}");
        for (incoming, (time, units, source)) in tokens.iter().zip(&indexed) {
            assert_eq!(
                (
                    u64::try_from(incoming.amount.units()).unwrap(),
                    incoming.from.to_string()
                ),
                (*units, source.clone()),
                "{protocol:?} {owner}"
            );
            assert!(incoming.time.abs_diff(*time) < 60, "{protocol:?} {owner}");
        }
        usdt_seen += tokens.len();
    }
    assert!(usdt_seen > 0);
}

async fn indexed_transaction(account: &str, lt: u64) -> serde_json::Value {
    let listed: serde_json::Value = reqwest::get(format!(
        "https://toncenter.com/api/v3/transactions?account={account}&lt={lt}&limit=1"
    ))
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    listed["transactions"][0].clone()
}

#[tokio::test]
#[ignore = "talks to the real TON network, run with --test-threads=1"]
async fn live_usdt_quotes_match_what_the_network_charged() {
    let nano = |value: &serde_json::Value| value.as_str().unwrap().parse::<u64>().unwrap();
    let sent = indexed_transaction(SENDER, SENT_LT).await;
    assert_eq!(nano(&sent["description"]["compute_ph"]["gas_used"]), 4_939);
    tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
    let ours = "0:b7f4010ccc8540eb958f3d228284ca379595d3fa52e47072bc1ee781d35e01e7";
    let first = indexed_transaction(ours, 109_578_663_000_003).await;
    let attached = nano(&first["in_msg"]["value"]);
    tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
    let refund = indexed_transaction(SENDER, 109_578_663_000_008).await;
    assert_eq!(refund["in_msg"]["opcode"], "0xd53276db");
    let paid = attached - nano(&refund["in_msg"]["value"]) + nano(&refund["total_fees"]);
    let registry = Registry::builtin().unwrap();
    let ton = registry.get(&"ton".parse().unwrap()).unwrap().clone();
    let transfer = bahaa_chain::Transfer {
        account: 0,
        from: Address::parse(ChainFamily::Ton, SENDER).unwrap(),
        to: Address::parse(ChainFamily::Ton, HOLDER).unwrap(),
        asset: format!("ton/{USDT}").parse().unwrap(),
        amount: Amount::parse("1", 6).unwrap(),
    };
    let mut seen = Vec::new();
    for protocol in [Protocol::TonLiteservers, Protocol::TonApi] {
        tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
        let mut network = ton.clone();
        network
            .endpoints
            .retain(|endpoint| endpoint.protocol == protocol);
        let (_, quote) = TonChain::new(network)
            .unwrap()
            .prepare(&transfer)
            .await
            .unwrap();
        let contracts = quote.lines()[3].clone();
        assert_eq!(contracts.item, bahaa_chain::FeeItem::Contracts);
        let quoted = u64::try_from(contracts.amount.units()).unwrap();
        assert!(
            quoted.abs_diff(paid) * 20 < paid,
            "{protocol:?}: quoted {quoted}, the network charged {paid}"
        );
        seen.push(quoted);
    }
    assert!(seen[0].abs_diff(seen[1]) * 100 < seen[0], "{seen:?}");
}
