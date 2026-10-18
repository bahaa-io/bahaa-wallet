#![allow(clippy::unwrap_used)]

use std::fmt::Write;
use std::net::TcpListener;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use alloy_consensus::{SignableTransaction, TxEnvelope};
use alloy_eips::eip2718::Decodable2718;
use bahaa_chain::{Chain, ChainError, FeeItem, Transfer, TxId, TxStatus, check_signed};
use bahaa_config::{ChainKind, Endpoint, Feature, Protocol, Registry};
use bahaa_crypto::{Mnemonic, Seed};
use bahaa_evm::{EvmChain, ScanCursor};
use bahaa_types::{Address, Amount, AssetId};
use serde_json::{Value, json};
use sha3::{Digest, Keccak256};
use url::Url;

const HARDHAT: &str = "test test test test test test test test test test test junk";
const USDT: &str = "0x55d398326f99059fF775485246999027B3197955";

struct Anvil {
    child: Child,
    url: Url,
    network: String,
}

impl Anvil {
    fn start(network: &str, extra: &[&str]) -> Self {
        let binary = std::env::var("BAHAA_ANVIL").unwrap();
        let registry = Registry::builtin().unwrap();
        let ChainKind::Evm { chain_id, .. } = registry.get(&network.parse().unwrap()).unwrap().kind
        else {
            panic!("not evm")
        };
        let port = TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let mut command = Command::new(binary);
        command
            .args(["--chain-id", &chain_id.to_string()])
            .args(["--port", &port.to_string(), "--silent"])
            .args(extra)
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        Self {
            child: command.spawn().unwrap(),
            url: Url::parse(&format!("http://127.0.0.1:{port}/")).unwrap(),
            network: network.to_owned(),
        }
    }

    async fn ready(&self) {
        let http = reqwest::Client::new();
        for _ in 0..100 {
            if self.rpc(&http, "eth_chainId", json!([])).await.is_some() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!("anvil did not start");
    }

    async fn rpc(&self, http: &reqwest::Client, method: &str, params: Value) -> Option<Value> {
        let body = json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params});
        let reply: Value = http
            .post(self.url.clone())
            .json(&body)
            .send()
            .await
            .ok()?
            .json()
            .await
            .ok()?;
        reply.get("result").cloned()
    }

    fn chain(&self) -> EvmChain {
        let registry = Registry::builtin().unwrap();
        let mut network = registry
            .get(&self.network.parse().unwrap())
            .unwrap()
            .clone();
        network.endpoints = vec![Endpoint {
            protocol: Protocol::JsonRpc,
            url: self.url.clone(),
            features: vec![Feature::Logs],
            min_gap_ms: 0,
        }];
        EvmChain::new(network).unwrap()
    }
}

impl Drop for Anvil {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn seed() -> Seed {
    Mnemonic::parse(HARDHAT).unwrap().to_seed("")
}

fn transfer(chain: &EvmChain, asset: AssetId, amount: Amount) -> Transfer {
    Transfer {
        account: 0,
        from: chain.derive(&seed(), 0).unwrap(),
        to: chain.derive(&seed(), 1).unwrap(),
        asset,
        amount,
    }
}

async fn send(chain: &EvmChain, transfer: &Transfer) -> (TxId, Amount, Amount) {
    let (plan, quote) = chain.prepare(transfer).await.unwrap();
    let signed = chain.sign(&plan, &seed()).unwrap();
    check_signed(transfer, &chain.decode(&signed).unwrap()).unwrap();
    let id = chain.broadcast(&signed).await.unwrap();
    assert_eq!(chain.broadcast(&signed).await.unwrap(), id);
    (id, quote.expected(), quote.max())
}

async fn settled(chain: &EvmChain, id: &TxId) -> Option<Amount> {
    for _ in 0..50 {
        match chain.status(id).await.unwrap() {
            TxStatus::Confirmed { fee, .. } => return fee,
            TxStatus::Failed(reason) => panic!("failed: {reason}"),
            _ => tokio::time::sleep(Duration::from_millis(100)).await,
        }
    }
    panic!("not confirmed");
}

async fn confirmed(chain: &EvmChain, id: &TxId) -> Amount {
    settled(chain, id).await.unwrap()
}

async fn balance(chain: &EvmChain, address: &Address, asset: &AssetId) -> Amount {
    chain
        .balances(address, std::slice::from_ref(asset))
        .await
        .unwrap()[0]
        .amount
}

#[tokio::test]
#[ignore = "needs a local anvil: BAHAA_ANVIL=/path/to/anvil"]
async fn sends_native_coins_end_to_end() {
    let anvil = Anvil::start("bsc", &[]);
    anvil.ready().await;
    let chain = anvil.chain();
    let bnb: AssetId = "bsc".parse().unwrap();
    let amount = Amount::parse("0.25", 18).unwrap();
    let transfer = transfer(&chain, bnb.clone(), amount);
    let before_from = balance(&chain, &transfer.from, &bnb).await;
    let before_to = balance(&chain, &transfer.to, &bnb).await;
    let (id, expected, max) = send(&chain, &transfer).await;
    let paid = confirmed(&chain, &id).await;
    assert!(paid <= max, "paid {paid} above max {max}");
    assert!(expected <= max);
    assert_eq!(
        balance(&chain, &transfer.to, &bnb).await,
        before_to.checked_add(amount).unwrap()
    );
    let spent = before_from
        .checked_sub(balance(&chain, &transfer.from, &bnb).await)
        .unwrap();
    assert_eq!(spent, amount.checked_add(paid).unwrap());
    let (second, _, _) = send(&chain, &transfer).await;
    confirmed(&chain, &second).await;
    assert_ne!(second, id);
}

#[tokio::test]
#[ignore = "needs a local anvil: BAHAA_ANVIL=/path/to/anvil"]
async fn refuses_to_overspend_and_knows_unknown_ids() {
    let anvil = Anvil::start("bsc", &[]);
    anvil.ready().await;
    let chain = anvil.chain();
    let empty = Mnemonic::from_entropy(&[3; 16]).unwrap().to_seed("");
    let mut transfer = transfer(
        &chain,
        "bsc".parse().unwrap(),
        Amount::parse("1", 18).unwrap(),
    );
    transfer.from = chain.derive(&empty, 0).unwrap();
    assert!(matches!(
        chain.prepare(&transfer).await,
        Err(ChainError::Insufficient { .. })
    ));
    let unknown = TxId::new(format!("0x{}", "ab".repeat(32))).unwrap();
    assert_eq!(chain.status(&unknown).await.unwrap(), TxStatus::NotFound);
}

#[tokio::test]
#[ignore = "needs a local anvil and internet: BAHAA_ANVIL=/path/to/anvil"]
async fn sends_real_bsc_usdt_on_a_forked_chain() {
    let anvil = Anvil::start("bsc", &["--fork-url", "https://bsc-dataseed.bnbchain.org"]);
    anvil.ready().await;
    let chain = anvil.chain();
    let http = reqwest::Client::new();
    let forked_at = anvil
        .rpc(&http, "eth_blockNumber", json!([]))
        .await
        .unwrap();
    let forked_at =
        u64::from_str_radix(forked_at.as_str().unwrap().trim_start_matches("0x"), 16).unwrap();
    let usdt: AssetId = format!("bsc/{USDT}").parse().unwrap();
    let from = chain.derive(&seed(), 0).unwrap();
    let Address::Evm(owner) = from else {
        panic!("not evm")
    };
    let funded = Amount::parse("1000", 18).unwrap();
    let mut slot_found = false;
    for slot in 0u8..20 {
        let mut key = [0u8; 64];
        key[12..32].copy_from_slice(&owner);
        key[63] = slot;
        let storage = Keccak256::digest(key)
            .iter()
            .fold(String::from("0x"), |mut out, byte| {
                let _ = write!(out, "{byte:02x}");
                out
            });
        let value = format!("0x{:064x}", funded.units());
        anvil
            .rpc(&http, "anvil_setStorageAt", json!([USDT, storage, value]))
            .await
            .unwrap();
        if balance(&chain, &from, &usdt).await == funded {
            slot_found = true;
            break;
        }
    }
    assert!(slot_found, "could not find the USDT balance slot");
    let amount = Amount::parse("2.5", 18).unwrap();
    let transfer = transfer(&chain, usdt.clone(), amount);
    let before_to = balance(&chain, &transfer.to, &usdt).await;
    let (id, _, max) = send(&chain, &transfer).await;
    assert!(confirmed(&chain, &id).await <= max);
    assert_eq!(
        balance(&chain, &transfer.to, &usdt).await,
        before_to.checked_add(amount).unwrap()
    );
    assert_eq!(
        balance(&chain, &transfer.from, &usdt).await,
        funded.checked_sub(amount).unwrap()
    );
    anvil
        .rpc(&http, "anvil_mine", json!(["0xc"]))
        .await
        .unwrap();
    let local_only = ScanCursor {
        next: forked_at + 1,
        complete_from: forked_at + 1,
        complete_since: 0,
    };
    let scanned = chain
        .scan(&transfer.to, std::slice::from_ref(&usdt), Some(local_only))
        .await
        .unwrap()
        .unwrap();
    let seen = scanned.found.iter().find(|entry| entry.id == id).unwrap();
    assert_eq!(
        (seen.from, seen.to, seen.amount),
        (transfer.from, transfer.to, amount)
    );
    assert_eq!(seen.asset, usdt);
    assert!(seen.time > 1_700_000_000);
}

#[tokio::test]
#[ignore = "needs a local anvil and internet: BAHAA_ANVIL=/path/to/anvil"]
async fn sends_on_a_forked_base_priced_by_the_real_fee_oracle() {
    let anvil = Anvil::start(
        "base",
        &[
            "--network",
            "base",
            "--fork-url",
            "https://mainnet.base.org",
        ],
    );
    anvil.ready().await;
    let chain = anvil.chain();
    let eth: AssetId = "base".parse().unwrap();
    let amount = Amount::parse("0.25", 18).unwrap();
    let mut transfer = transfer(&chain, eth.clone(), amount);
    let stranger = Mnemonic::from_entropy(&[7; 16]).unwrap().to_seed("");
    transfer.to = chain.derive(&stranger, 0).unwrap();
    let before_from = balance(&chain, &transfer.from, &eth).await;
    let (plan, quote) = chain.prepare(&transfer).await.unwrap();
    let [gas, l1] = quote.lines() else {
        panic!("unexpected fee lines: {quote:?}");
    };
    assert!(matches!(gas.item, FeeItem::Gas { units: 21_000, .. }));
    assert_eq!(l1.item, FeeItem::L1Data);
    assert!(!l1.amount.is_zero());
    let signed = chain.sign(&plan, &seed()).unwrap();
    check_signed(&transfer, &chain.decode(&signed).unwrap()).unwrap();
    let envelope = TxEnvelope::decode_2718(&mut &signed.bytes[..]).unwrap();
    let size = envelope
        .as_eip1559()
        .unwrap()
        .tx()
        .encoded_for_signing()
        .len();
    let http = reqwest::Client::new();
    let bound = anvil
        .rpc(
            &http,
            "eth_call",
            json!([{
                "to": "0x420000000000000000000000000000000000000F",
                "data": format!("0xf1c7a58b{size:064x}")
            }, "latest"]),
        )
        .await
        .unwrap();
    let bound = u128::from_str_radix(bound.as_str().unwrap().trim_start_matches("0x"), 16).unwrap();
    assert!(l1.amount <= Amount::new(bound.try_into().unwrap(), 18).unwrap());
    let id = chain.broadcast(&signed).await.unwrap();
    assert_eq!(settled(&chain, &id).await, None);
    assert_eq!(balance(&chain, &transfer.to, &eth).await, amount);
    let spent = before_from
        .checked_sub(balance(&chain, &transfer.from, &eth).await)
        .unwrap();
    assert!(spent > amount && spent <= amount.checked_add(quote.max()).unwrap());
}
