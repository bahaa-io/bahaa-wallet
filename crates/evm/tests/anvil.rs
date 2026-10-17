#![allow(clippy::unwrap_used)]

use std::fmt::Write;
use std::net::TcpListener;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use bahaa_chain::{Chain, ChainError, Transfer, TxId, TxStatus, check_signed};
use bahaa_config::{Endpoint, Feature, Protocol, Registry};
use bahaa_crypto::{Mnemonic, Seed};
use bahaa_evm::EvmChain;
use bahaa_types::{Address, Amount, AssetId};
use serde_json::{Value, json};
use sha3::{Digest, Keccak256};
use url::Url;

const HARDHAT: &str = "test test test test test test test test test test test junk";
const USDT: &str = "0x55d398326f99059fF775485246999027B3197955";

struct Anvil {
    child: Child,
    url: Url,
}

impl Anvil {
    fn start(fork: Option<&str>) -> Self {
        let binary = std::env::var("BAHAA_ANVIL").unwrap();
        let port = TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let mut command = Command::new(binary);
        command
            .args(["--chain-id", "56", "--port", &port.to_string(), "--silent"])
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        if let Some(fork) = fork {
            command.args(["--fork-url", fork]);
        }
        Self {
            child: command.spawn().unwrap(),
            url: Url::parse(&format!("http://127.0.0.1:{port}/")).unwrap(),
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
        let mut network = registry.get(&"bsc".parse().unwrap()).unwrap().clone();
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

async fn confirmed(chain: &EvmChain, id: &TxId) -> Amount {
    for _ in 0..50 {
        match chain.status(id).await.unwrap() {
            TxStatus::Confirmed { fee: Some(fee), .. } => return fee,
            TxStatus::Failed(reason) => panic!("failed: {reason}"),
            _ => tokio::time::sleep(Duration::from_millis(100)).await,
        }
    }
    panic!("not confirmed");
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
    let anvil = Anvil::start(None);
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
    let anvil = Anvil::start(None);
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
    let anvil = Anvil::start(Some("https://bsc-dataseed.bnbchain.org"));
    anvil.ready().await;
    let chain = anvil.chain();
    let usdt: AssetId = format!("bsc/{USDT}").parse().unwrap();
    let from = chain.derive(&seed(), 0).unwrap();
    let Address::Evm(owner) = from else {
        panic!("not evm")
    };
    let funded = Amount::parse("1000", 18).unwrap();
    let http = reqwest::Client::new();
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
}
