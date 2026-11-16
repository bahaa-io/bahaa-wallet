#![allow(clippy::unwrap_used)]

mod common;

use std::time::Duration;

use bahaa_chain::abi::{self, Arg};
use bahaa_config::Registry;
use bahaa_engine::{Address, EngineError, NameService};
use bahaa_types::BitcoinScript;
use common::{Scratch, config, result, unhex};
use serde_json::{Value, json};
use sha3::{Digest, Keccak256};

fn word(value: usize) -> String {
    format!("{value:064x}")
}

fn bytes(data: &[u8]) -> String {
    let mut out = word(data.len());
    out.push_str(&common::hex(data));
    out.push_str(&"00".repeat(data.len().div_ceil(32) * 32 - data.len()));
    out
}

fn coin_of(data: &[u8]) -> u64 {
    let args = &data[4..];
    let read = |at: usize| {
        usize::try_from(u64::from_be_bytes(
            args[at + 24..at + 32].try_into().unwrap(),
        ))
        .unwrap()
    };
    let start = read(32);
    let length = read(start);
    let query = &args[start + 32..start + 32 + length];
    u64::from_be_bytes(query[query.len() - 8..].try_into().unwrap())
}

const REVERSE: [u8; 4] = [0x5d, 0x78, 0xa2, 0x17];

fn primary_name(data: &[u8]) -> Value {
    let looked_up = &data[4 + 32 * 3..4 + 32 * 3 + 20];
    let name = match looked_up[0] {
        0xd8 | 0x11 => "vitalik.eth",
        _ => "",
    };
    let head = format!("{}{}{}", word(96), word(0x4976), word(0x4977));
    result(json!(format!("0x{head}{}", bytes(name.as_bytes()))))
}

fn answer(call: &Value) -> Value {
    match call["method"].as_str().unwrap() {
        "eth_chainId" => result(json!("0x1")),
        "eth_call" => {
            let data = unhex(call["params"][0]["data"].as_str().unwrap());
            if data.starts_with(&REVERSE) {
                return primary_name(&data);
            }
            if data.starts_with(&selector("reverseNameOf(address)")) {
                return result(json!(format!("0x{}", common::hex(&text("")))));
            }
            let mut segwit = vec![0x00, 0x14];
            segwit.extend([0x22; 20]);
            let stored: Vec<u8> = match coin_of(&data) {
                60 => vec![0xd8; 20],
                0x8000_0000 => vec![0x11; 20],
                0 => segwit,
                _ => Vec::new(),
            };
            let inner = format!("{}{}", word(32), bytes(&stored));
            let reply = format!("0x{}{}{}", word(64), word(0x4976), bytes(&unhex(&inner)));
            result(json!(reply))
        }
        other => panic!("unexpected {other}"),
    }
}

#[tokio::test]
async fn names_resolve_to_the_record_of_the_asked_network() {
    let scratch = Scratch::new();
    let url = common::node(|call| (answer(call), Duration::ZERO)).await;
    let builtin = Registry::builtin().unwrap();
    let kept = |id: &str| builtin.get(&id.parse().unwrap()).unwrap().clone();
    let registry = Registry::with_networks(vec![
        common::network("ethereum", url.clone()),
        common::network("base", url),
        kept("bitcoin"),
        kept("solana"),
        kept("ton"),
    ])
    .unwrap();
    let engine = bahaa_engine::Engine::start(config(&scratch, registry, None)).unwrap();
    let resolve =
        |name: &str, network: &str| engine.resolve_name(name.to_owned(), network.parse().unwrap());
    let on_ethereum = resolve("VITALIK.eth", "ethereum").await.unwrap();
    assert_eq!(on_ethereum.name, "vitalik.eth");
    assert_eq!(on_ethereum.address, Address::Evm([0xd8; 20]));
    assert_eq!(on_ethereum.service, NameService::Ens);
    assert_eq!(
        resolve("vitalik.eth", "base").await.unwrap().address,
        Address::Evm([0x11; 20])
    );
    assert_eq!(
        resolve("vitalik.eth", "bitcoin").await.unwrap().address,
        Address::Bitcoin {
            script: BitcoinScript::WitnessKeyHash([0x22; 20]),
            testnet: false,
        }
    );
    let missing = resolve("vitalik.eth", "solana").await.unwrap_err();
    assert!(missing.to_string().contains("no address"), "{missing}");
    assert!(matches!(
        resolve("vitalik.eth", "ton").await,
        Err(EngineError::Chain(bahaa_engine::ChainError::Unsupported(_)))
    ));
    assert!(resolve("vit alik.eth", "ethereum").await.is_err());
    let ton_elsewhere = resolve("foundation.ton", "base").await.unwrap_err();
    assert!(
        ton_elsewhere.to_string().contains("TON name"),
        "{ton_elsewhere}"
    );
    let solana_elsewhere = resolve("bonfida.sns", "bitcoin").await.unwrap_err();
    assert!(
        solana_elsewhere.to_string().contains("Solana name"),
        "{solana_elsewhere}"
    );
    let bnb_elsewhere = resolve("spaceid.bnb", "ethereum").await.unwrap_err();
    assert!(
        bnb_elsewhere.to_string().contains("BNB Chain"),
        "{bnb_elsewhere}"
    );
    assert!(matches!(
        resolve("brad.crypto", "ethereum").await,
        Err(EngineError::Chain(bahaa_engine::ChainError::Unsupported(_)))
    ));
    assert_eq!(
        resolve("shop.travel", "ethereum").await.unwrap().service,
        NameService::Ens
    );
    let bitcoin_elsewhere = resolve("matt@mattcorallo.com", "base").await.unwrap_err();
    assert!(
        bitcoin_elsewhere.to_string().contains("Bitcoin name"),
        "{bitcoin_elsewhere}"
    );
    let paused = resolve("bonfida.sol", "solana").await.unwrap_err();
    assert!(paused.to_string().contains(".sns"), "{paused}");
    assert!(matches!(
        resolve("vitalik.eth", "polygon").await,
        Err(EngineError::UnknownNetwork)
    ));
}

#[tokio::test]
async fn a_name_behind_an_address_shows_only_when_it_points_back() {
    let scratch = Scratch::new();
    let url = common::node(|call| (answer(call), Duration::ZERO)).await;
    let registry = Registry::with_networks(vec![common::network("ethereum", url)]).unwrap();
    let engine = bahaa_engine::Engine::start(config(&scratch, registry, None)).unwrap();
    let name_of =
        |address: [u8; 20]| engine.name_of("ethereum".parse().unwrap(), Address::Evm(address));
    let found = name_of([0xd8; 20]).await.unwrap().unwrap();
    assert_eq!(found.name, "vitalik.eth");
    assert_eq!(found.service, NameService::Ens);
    assert_eq!(name_of([0x11; 20]).await.unwrap(), None);
    assert_eq!(name_of([0x33; 20]).await.unwrap(), None);
    engine
        .create(bahaa_engine::SecretString::from("pw"))
        .await
        .unwrap();
    engine
        .set_settings(bahaa_engine::Settings {
            name_lookups: false,
            ..bahaa_engine::Settings::default()
        })
        .await
        .unwrap();
    assert_eq!(name_of([0xd8; 20]).await.unwrap(), None);
}

const ALICE: [u8; 20] = [0xa1; 20];
const BOB: [u8; 20] = [0xb2; 20];
const CAROL: [u8; 20] = [0xc3; 20];
const DAVE: [u8; 20] = [0xd4; 20];
const REVERSE_RESOLVER: [u8; 20] = [0x5a; 20];

fn selector(signature: &str) -> [u8; 4] {
    let hash = Keccak256::digest(signature.as_bytes());
    [hash[0], hash[1], hash[2], hash[3]]
}

fn namehash(name: &str) -> [u8; 32] {
    name.rsplit('.').fold([0; 32], |node, label| {
        let mut joined = node.to_vec();
        joined.extend(Keccak256::digest(label.as_bytes()));
        Keccak256::digest(&joined).into()
    })
}

fn padded(address: &[u8; 20]) -> Vec<u8> {
    let mut word = vec![0; 12];
    word.extend(address);
    word
}

fn text(value: &str) -> Vec<u8> {
    abi::encode(&[Arg::Bytes(value.as_bytes())])
}

fn dns_name(encoded: &[u8]) -> String {
    let mut labels = Vec::new();
    let mut rest = encoded;
    while let [length, tail @ ..] = rest {
        let length = usize::from(*length);
        if length == 0 {
            break;
        }
        labels.push(String::from_utf8(tail[..length].to_vec()).unwrap());
        rest = &tail[length..];
    }
    labels.join(".")
}

fn ens_records(data: &[u8]) -> Vec<u8> {
    let args = &data[4..];
    let name = dns_name(abi::bytes_at(args, 0).unwrap());
    let query = abi::bytes_at(args, 1).unwrap();
    let coin = abi::uint(&query[4..], 1).unwrap();
    let stored = match (name.as_str(), coin.to::<u64>()) {
        ("alice.base.eth", 0x8000_2105) => ALICE.to_vec(),
        ("bob.base.eth", 0x8000_2105) => vec![0x99; 20],
        _ => Vec::new(),
    };
    let result = abi::encode(&[Arg::Bytes(&stored)]);
    let mut reply = abi::encode(&[Arg::Bytes(&result), Arg::Bytes(&[])]);
    reply.splice(32..64, padded(&[0x49; 20]));
    reply
}

fn owner_of(data: &[u8]) -> [u8; 20] {
    abi::address(&data[4..], 0).unwrap()
}

fn chain_answer(chain: u64, call: &Value) -> Value {
    let method = call["method"].as_str().unwrap();
    if method == "eth_chainId" {
        return result(json!(format!("0x{chain:x}")));
    }
    assert_eq!(method, "eth_call");
    let data = unhex(call["params"][0]["data"].as_str().unwrap());
    let node = data.get(4..36).unwrap_or_default();
    let reply = if data.starts_with(&selector("reverse(bytes,uint256)")) {
        text("")
    } else if data.starts_with(&selector("resolve(bytes,bytes)")) {
        ens_records(&data)
    } else if data.starts_with(&selector("nameForAddr(address)")) {
        text(match owner_of(&data) {
            ALICE => "alice.base.eth",
            BOB => "bob.base.eth",
            _ => "",
        })
    } else if data.starts_with(&selector("reverseNameOf(address)")) {
        text(if chain == 137 && owner_of(&data) == DAVE {
            "dave.crypto"
        } else {
            ""
        })
    } else if data.starts_with(&selector("resolver(bytes32)")) {
        let reverse = namehash(&format!("{}.addr.reverse", common::hex(&CAROL)));
        let found = if node == reverse {
            REVERSE_RESOLVER
        } else if node == namehash("carol.bnb") {
            [0x5b; 20]
        } else {
            [0; 20]
        };
        padded(&found)
    } else if data.starts_with(&selector("name(bytes32)")) {
        text("carol.bnb")
    } else if data.starts_with(&selector("owner(bytes32)"))
        || data.starts_with(&selector("addr(bytes32)"))
    {
        padded(&CAROL)
    } else if data.starts_with(&selector("nameExpires(uint256)")) {
        unhex(&word(4_000_000_000))
    } else if data.starts_with(&selector("multicall(bytes[])")) {
        let answers = [
            padded(&DAVE),
            unhex(&word(1)),
            text(&format!("0x{}", common::hex(&DAVE))),
        ];
        abi::encode(&[Arg::List(&answers)])
    } else {
        panic!("unexpected call {}", common::hex(&data[..4]))
    };
    result(json!(format!("0x{}", common::hex(&reply))))
}

#[tokio::test]
async fn names_behind_addresses_come_from_each_system_and_must_point_back() {
    let scratch = Scratch::new();
    let mut networks = Vec::new();
    for (id, chain) in [
        ("ethereum", 1),
        ("base", 8453),
        ("bsc", 56),
        ("polygon", 137),
    ] {
        let url = common::node(move |call| (chain_answer(chain, call), Duration::ZERO)).await;
        networks.push(common::network(id, url));
    }
    let registry = Registry::with_networks(networks).unwrap();
    let engine = bahaa_engine::Engine::start(config(&scratch, registry, None)).unwrap();
    let name_of = |network: &str, address: [u8; 20]| {
        engine.name_of(network.parse().unwrap(), Address::Evm(address))
    };
    let alice = name_of("base", ALICE).await.unwrap().unwrap();
    assert_eq!(
        (alice.name.as_str(), alice.service),
        ("alice.base.eth", NameService::Ens)
    );
    assert_eq!(name_of("base", BOB).await.unwrap(), None);
    let carol = name_of("bsc", CAROL).await.unwrap().unwrap();
    assert_eq!(
        (carol.name.as_str(), carol.service),
        ("carol.bnb", NameService::SpaceId)
    );
    let dave = name_of("bsc", DAVE).await.unwrap().unwrap();
    assert_eq!(
        (dave.name.as_str(), dave.service),
        ("dave.crypto", NameService::Unstoppable)
    );
    assert_eq!(name_of("polygon", ALICE).await.unwrap(), None);
}

#[tokio::test]
#[ignore = "talks to the real Ethereum, Base, BNB Chain, Arbitrum, Polygon and Solana"]
async fn live_names_behind_addresses_point_back() {
    let scratch = Scratch::new();
    let engine =
        bahaa_engine::Engine::start(config(&scratch, Registry::builtin().unwrap(), None)).unwrap();
    let evm = |text: &str| Address::Evm(bahaa_types::hex::decode_array(text).unwrap());
    let spaceid = evm("b5932a6b7d50a966aec6c74c97385412fb497540");
    for (network, address, expected) in [
        (
            "ethereum",
            evm("d8da6bf26964af9d7eed9e03e53415d37aa96045"),
            "vitalik.eth",
        ),
        (
            "base",
            evm("2211d1d0020daea8039e46cf1367962070d77da9"),
            "jesse.base.eth",
        ),
        ("bsc", spaceid, "spaceid.bnb"),
        ("arbitrum", spaceid, "spaceid.arb"),
        (
            "solana",
            Address::parse(
                bahaa_types::ChainFamily::Solana,
                "Fw1ETanDZafof7xEULsnq9UY6o71Tpds89tNwPkWLb1v",
            )
            .unwrap(),
            "couponvault.sns",
        ),
    ] {
        let found = engine
            .name_of(network.parse().unwrap(), address)
            .await
            .unwrap();
        assert_eq!(
            found.map(|found| found.name).as_deref(),
            Some(expected),
            "{network}"
        );
    }
    let brad = evm("8aad44321a86b170879d7a244c1e8d360c99dda8");
    for (network, expected) in [
        ("polygon", Some("brad.x")),
        ("ethereum", Some("brad.x")),
        ("base", None),
    ] {
        let found = engine
            .name_of(network.parse().unwrap(), brad)
            .await
            .unwrap();
        assert_eq!(
            found.map(|found| found.name).as_deref(),
            expected,
            "{network}"
        );
    }
}
