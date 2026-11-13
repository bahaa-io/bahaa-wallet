#![allow(clippy::unwrap_used)]

mod common;

use std::time::Duration;

use bahaa_config::Registry;
use bahaa_engine::{Address, EngineError, NameService};
use bahaa_types::BitcoinScript;
use common::{Scratch, config, result, unhex};
use serde_json::{Value, json};

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

fn answer(call: &Value) -> Value {
    match call["method"].as_str().unwrap() {
        "eth_chainId" => result(json!("0x1")),
        "eth_call" => {
            let data = unhex(call["params"][0]["data"].as_str().unwrap());
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
    assert!(matches!(
        resolve("vitalik.eth", "polygon").await,
        Err(EngineError::UnknownNetwork)
    ));
}
