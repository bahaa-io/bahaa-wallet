#![allow(clippy::unwrap_used)]

mod common;

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use bahaa_chain::abi::{self, Arg};
use bahaa_chain::{ChainError, Link, Step};
use bahaa_config::{Feature, Network, Registry};
use bahaa_evm::EvmChain;
use bahaa_types::{Address, ChainFamily, hex};
use common::{Log, OWNER, node, result, word};
use ruint::aliases::U256;
use serde_json::{Value, json};
use sha3::{Digest, Keccak256};

const RESOLVER: [u8; 20] = [0x23; 20];
const UNIVERSAL: &str = "eeeeeeee14d718c2b47d9923deab1335e144eeee";
const ELSEWHERE: [u8; 20] = [0x11; 20];

fn selector(signature: &str) -> [u8; 4] {
    let hash = Keccak256::digest(signature.as_bytes());
    [hash[0], hash[1], hash[2], hash[3]]
}

fn owner_bytes() -> [u8; 20] {
    hex::decode_array(&OWNER[2..].to_lowercase()).unwrap()
}

fn unhex(value: &Value) -> Vec<u8> {
    hex::decode(&value.as_str().unwrap()[2..]).unwrap()
}

fn padded(address: &[u8; 20]) -> String {
    format!("0x{:0>64}", hex::encode(address))
}

#[derive(Clone, Default)]
struct Name {
    records: BTreeMap<u64, Vec<u8>>,
    primary: Option<String>,
}

impl Name {
    fn apply(&mut self, data: &[u8]) {
        if data.starts_with(&selector("multicall(bytes[])")) {
            for item in abi::bytes_list(&data[4..], 0).unwrap() {
                self.apply(item);
            }
        } else if data.starts_with(&selector("setAddr(bytes32,uint256,bytes)")) {
            let coin = u64::try_from(abi::uint(&data[4..], 1).unwrap()).unwrap();
            let value = abi::bytes_at(&data[4..], 2).unwrap().to_vec();
            self.records.insert(coin, value);
        } else if data.starts_with(&selector("setAddr(bytes32,address)")) {
            self.records
                .insert(60, abi::address(&data[4..], 1).unwrap().to_vec());
        } else if data.starts_with(&selector("setName(string)")) {
            self.primary = Some(abi::abi_string(&data[4..], 255).unwrap());
        }
    }

    fn read(&self, data: &[u8]) -> Vec<u8> {
        if data.starts_with(&selector("reverse(bytes,uint256)"))
            || data.starts_with(&selector("nameForAddr(address)"))
        {
            let name = self.primary.clone().unwrap_or_default();
            let mut reply = abi::encode(&[Arg::Bytes(name.as_bytes())]);
            if data.starts_with(&selector("reverse(bytes,uint256)")) {
                reply.splice(..32, U256::from(96u8).to_be_bytes::<32>());
                reply.splice(32..32, [0u8; 64]);
            }
            return reply;
        }
        let batch = abi::bytes_at(&data[4..], 1).unwrap();
        let answers: Vec<Vec<u8>> = abi::bytes_list(&batch[4..], 0)
            .unwrap()
            .iter()
            .map(|query| {
                let coin = u64::try_from(abi::uint(&query[4..], 1).unwrap()).unwrap();
                let stored = self.records.get(&coin).cloned().unwrap_or_default();
                abi::encode(&[Arg::Bytes(&stored)])
            })
            .collect();
        let result = abi::encode(&[Arg::List(&answers)]);
        let mut reply = abi::encode(&[Arg::Bytes(&result), Arg::Uint(U256::ZERO)]);
        reply[32 + 12..64].copy_from_slice(&RESOLVER);
        reply
    }
}

fn answer(name: &Arc<Mutex<Name>>, lying: bool, method: &str, params: &Value) -> (u16, Value) {
    match method {
        "eth_chainId" => result(&json!("0x1")),
        "eth_call" => {
            let data = unhex(&params[0]["data"]);
            let to = params[0]["to"].as_str().unwrap().to_lowercase();
            let reply = if to.ends_with(UNIVERSAL) {
                format!("0x{}", hex::encode(&name.lock().unwrap().read(&data)))
            } else if data.starts_with(&selector("getState(uint256)")) {
                format!("0x{}", "00".repeat(31) + "01" + &"00".repeat(128))
            } else if data.starts_with(&selector("owner(bytes32)")) {
                padded(&owner_bytes())
            } else if data.starts_with(&selector("resolver(bytes32)")) {
                padded(&RESOLVER)
            } else {
                word(4_000_000_000)
            };
            result(&json!(reply))
        }
        "eth_simulateV1" => {
            let mut after = name.lock().unwrap().clone();
            let calls = params[0]["blockStateCalls"][0]["calls"].as_array().unwrap();
            let ran: Vec<Value> = calls
                .iter()
                .map(|call| {
                    let data = unhex(&call["data"]);
                    let from = call["from"].as_str().unwrap().to_lowercase();
                    let to = call["to"].as_str().unwrap().to_lowercase();
                    let writing = call.get("value").is_some();
                    let (ok, output) = if !writing {
                        (true, after.read(&data))
                    } else if from == OWNER.to_lowercase() || !to.ends_with(&hex::encode(&RESOLVER))
                    {
                        if !lying {
                            after.apply(&data);
                        }
                        (true, Vec::new())
                    } else {
                        (false, Vec::new())
                    };
                    json!({
                        "status": if ok { "0x1" } else { "0x0" },
                        "gasUsed": "0xc350",
                        "returnData": format!("0x{}", hex::encode(&output)),
                        "logs": [],
                    })
                })
                .collect();
            result(&json!([{"calls": ran}]))
        }
        "eth_feeHistory" => result(&json!({"baseFeePerGas": ["0x3b9aca00", "0x3b9aca00"]})),
        "eth_maxPriorityFeePerGas" => result(&json!("0x3b9aca00")),
        "eth_getTransactionCount" => result(&json!("0x5")),
        "eth_getBalance" => result(&json!("0xde0b6b3a7640000")),
        _ => (
            200,
            json!({"jsonrpc": "2.0", "id": 1, "error": {"code": -32601, "message": "no such method"}}),
        ),
    }
}

async fn ethereum(lying: bool) -> (EvmChain, Vec<Network>) {
    let mut held = Name::default();
    held.records.insert(60, owner_bytes().to_vec());
    let name = Arc::new(Mutex::new(held));
    let log: Log = Arc::default();
    let url = node(log, move |method, params| {
        answer(&name, lying, method, params)
    })
    .await;
    let mut network = common::network_on("ethereum", &[url]);
    for endpoint in &mut network.endpoints {
        endpoint.features.push(Feature::Simulate);
    }
    let covered = Registry::builtin()
        .unwrap()
        .networks()
        .filter(|network| !network.testnet && network.family() == ChainFamily::Evm)
        .cloned()
        .collect();
    (EvmChain::new(network).unwrap(), covered)
}

fn coins_written(data: &[u8]) -> Vec<(Vec<u8>, U256)> {
    abi::bytes_list(&data[4..], 0)
        .unwrap()
        .iter()
        .map(|set| (set[..4].to_vec(), abi::uint(&set[4..], 1).unwrap()))
        .collect()
}

#[tokio::test]
async fn an_ens_name_points_elsewhere_and_back_after_a_trial_run_proves_it() {
    let (chain, covered) = ethereum(false).await;
    let owner = Address::Evm(owner_bytes());
    let record = chain
        .find_name("Alice.eth", &covered)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(record.name, "alice.eth");
    assert_eq!(record.owner, owner);
    assert_eq!(record.wallet, Some(owner));
    assert_eq!(record.expires_at, Some(4_000_000_000));
    assert!(!record.renewable && record.elsewhere);

    let linking = chain
        .prepare_link(
            0,
            &owner,
            "alice.eth",
            Link::To(Address::Evm(ELSEWHERE)),
            &covered,
        )
        .await
        .unwrap();
    assert_eq!(linking.calls.len(), 1);
    let planned = linking.calls[0].planned();
    assert_eq!(
        planned.step,
        Step::Name {
            name: "alice.eth".to_owned()
        }
    );
    assert_eq!(planned.call.to, Address::Evm(RESOLVER));
    assert!(planned.call.value.is_zero());
    let set = selector("setAddr(bytes32,uint256,bytes)").to_vec();
    assert_eq!(
        coins_written(&planned.call.data),
        vec![
            (set.clone(), U256::from(60u8)),
            (set, U256::from(0x8000_0000u32)),
        ]
    );
    assert!(!linking.fee.max().is_zero());

    let cleared = chain
        .prepare_link(0, &owner, "alice.eth", Link::Clear, &covered)
        .await
        .unwrap();
    assert_eq!(
        coins_written(&cleared.calls[0].planned().call.data),
        vec![(selector("setAddr(bytes32,address)").to_vec(), U256::ZERO)]
    );

    let primary = chain
        .prepare_link(0, &owner, "alice.eth", Link::Primary, &covered)
        .await
        .unwrap();
    assert_eq!(primary.calls.len(), 2);
    assert_eq!(
        primary.calls[0]
            .planned()
            .call
            .to
            .to_string()
            .to_lowercase(),
        "0xa58e81fe9b61b5c3fe2afd33cf304c454abfc7cb"
    );
}

#[tokio::test]
async fn only_the_holder_may_change_a_name_and_a_change_that_doesnt_stick_is_refused() {
    let (chain, covered) = ethereum(false).await;
    let stranger = Address::Evm([0x77; 20]);
    let refused = chain
        .prepare_link(0, &stranger, "alice.eth", Link::Clear, &covered)
        .await;
    assert!(matches!(refused, Err(ChainError::Invalid(_))));
    let not_pointed = chain
        .prepare_link(0, &stranger, "alice.eth", Link::Primary, &covered)
        .await;
    assert!(matches!(not_pointed, Err(ChainError::Invalid(_))));
    let owner = Address::Evm(owner_bytes());
    let elsewhere = chain
        .prepare_link(0, &owner, "alice.eth", Link::To(owner), &covered)
        .await
        .unwrap();
    assert!(elsewhere.record.elsewhere);
    let set = selector("setAddr(bytes32,uint256,bytes)").to_vec();
    assert_eq!(
        coins_written(&elsewhere.calls[0].planned().call.data),
        vec![
            (set.clone(), U256::from(60u8)),
            (set, U256::from(0x8000_0000u32)),
        ]
    );
    assert!(matches!(
        chain
            .prepare_link(0, &owner, "alice.eth", Link::Renew, &covered)
            .await,
        Err(ChainError::Unsupported(_))
    ));

    let (lying, covered) = ethereum(true).await;
    let ignored = lying
        .prepare_link(
            0,
            &owner,
            "alice.eth",
            Link::To(Address::Evm(ELSEWHERE)),
            &covered,
        )
        .await;
    assert!(
        matches!(ignored, Err(ChainError::Rejected(_))),
        "{ignored:?}"
    );
}
