use std::time::Duration;

use alloy_consensus::transaction::SignerRecoverable;
use alloy_consensus::{SignableTransaction, TxEip1559, TxEnvelope};
use alloy_eips::eip2718::{Decodable2718, Encodable2718};
use alloy_primitives::utils::format_units;
use alloy_primitives::{Address, TxKind, U256, hex};
use alloy_signer::SignerSync;
use alloy_signer_local::PrivateKeySigner;
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};

use crate::keys;
use crate::prompt::confirm;

const RPC: &str = "https://bsc-testnet-rpc.publicnode.com";
const RPC_OVERRIDE: &str = "BAHAA_SPIKE_BSC_RPC";
const CHAIN_ID: u64 = 97;
const AMOUNT_WEI: u64 = 100_000_000_000_000;
const EXPLORER: &str = "https://testnet.bscscan.com/tx/";
const FAUCET: &str = "https://www.bnbchain.org/en/testnet-faucet";
const RECEIPT_TRIES: u32 = 60;

struct Rpc {
    http: reqwest::Client,
    url: String,
}

impl Rpc {
    fn new() -> Result<Self> {
        let http = reqwest::Client::builder()
            .user_agent(concat!("bahaa-spike/", env!("CARGO_PKG_VERSION")))
            .timeout(Duration::from_secs(15))
            .build()?;
        let url = std::env::var(RPC_OVERRIDE).unwrap_or_else(|_| RPC.to_owned());
        Ok(Self { http, url })
    }

    fn is_public(&self) -> bool {
        self.url == RPC
    }

    async fn call(&self, method: &str, params: Value) -> Result<Value> {
        let reply: Value = self
            .http
            .post(&self.url)
            .json(&json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}))
            .send()
            .await?
            .json()
            .await?;
        if let Some(error) = reply.get("error") {
            bail!("{method}: {error}");
        }
        reply
            .get("result")
            .cloned()
            .with_context(|| format!("{method}: reply without a result"))
    }
}

pub async fn transfer() -> Result<()> {
    let phrase = keys::load()?;
    let signer = keys::evm(&phrase)?;
    let from = signer.address();
    let rpc = Rpc::new()?;
    let chain_id = quantity(&rpc.call("eth_chainId", json!([])).await?)?;
    ensure!(
        chain_id == u128::from(CHAIN_ID),
        "endpoint is on chain {chain_id}, not BSC testnet"
    );
    let balance = big(&rpc
        .call("eth_getBalance", json!([from.to_string(), "latest"]))
        .await?)?;
    let amount = U256::from(AMOUNT_WEI);
    println!("bsc testnet  {from}");
    println!("endpoint     {}", rpc.url);
    println!("balance      {} tBNB", units(balance, 18)?);
    if balance <= amount {
        return fund(amount);
    }
    let nonce = u64::try_from(quantity(
        &rpc.call(
            "eth_getTransactionCount",
            json!([from.to_string(), "pending"]),
        )
        .await?,
    )?)?;
    let gas_price = quantity(&rpc.call("eth_gasPrice", json!([])).await?)?;
    let gas_limit = u64::try_from(quantity(
        &rpc.call(
            "eth_estimateGas",
            json!([{"from": from.to_string(), "to": from.to_string(), "value": format!("{amount:#x}")}]),
        )
        .await?,
    )?)?;
    let max_fee = U256::from(gas_limit) * U256::from(gas_price);
    if balance < amount + max_fee {
        return fund(amount + max_fee);
    }
    let tx = TxEip1559 {
        chain_id: CHAIN_ID,
        nonce,
        gas_limit,
        max_fee_per_gas: gas_price,
        max_priority_fee_per_gas: gas_price,
        to: TxKind::Call(from),
        value: amount,
        ..TxEip1559::default()
    };
    let raw = sign(&signer, tx)?;
    let decoded = TxEnvelope::decode_2718(&mut raw.as_slice())?;
    review(&decoded, from)?;
    if !confirm()? {
        println!("not sent");
        return Ok(());
    }
    let hash = decoded.tx_hash().to_string();
    let sent = rpc
        .call(
            "eth_sendRawTransaction",
            json!([hex::encode_prefixed(&raw)]),
        )
        .await?;
    ensure!(
        sent.as_str() == Some(hash.as_str()),
        "node answered with a different hash {sent}"
    );
    println!("sent         {hash}");
    let receipt = wait(&rpc, &hash).await?;
    let block = quantity(&receipt["blockNumber"])?;
    let used = quantity(&receipt["gasUsed"])?;
    let price = quantity(&receipt["effectiveGasPrice"])?;
    let status = if receipt["status"] == "0x1" {
        "success"
    } else {
        "failed"
    };
    println!(
        "confirmed    block {block}, {status}, fee paid {} tBNB",
        units(U256::from(used) * U256::from(price), 18)?
    );
    if rpc.is_public() {
        println!("explorer     {EXPLORER}{hash}");
    }
    Ok(())
}

fn sign(signer: &PrivateKeySigner, tx: TxEip1559) -> Result<Vec<u8>> {
    let signature = signer.sign_hash_sync(&tx.signature_hash())?;
    Ok(TxEnvelope::from(tx.into_signed(signature)).encoded_2718())
}

fn review(envelope: &TxEnvelope, expected: Address) -> Result<()> {
    let signer = envelope.recover_signer()?;
    ensure!(
        signer == expected,
        "signature recovers to {signer}, not {expected}"
    );
    let tx = envelope
        .as_eip1559()
        .context("not an EIP-1559 transaction")?
        .tx();
    ensure!(
        tx.chain_id == CHAIN_ID,
        "signed for chain {}, not BSC testnet",
        tx.chain_id
    );
    let to = tx.to.to().context("contract creation, not a transfer")?;
    let fee = U256::from(tx.gas_limit) * U256::from(tx.max_fee_per_gas);
    println!();
    println!("review, decoded from the signed bytes");
    println!("  network    BNB Smart Chain testnet ({})", tx.chain_id);
    println!("  from       {signer}");
    println!("  to         {to}");
    println!("  amount     {} tBNB", units(tx.value, 18)?);
    println!("  nonce      {}", tx.nonce);
    println!(
        "  max fee    {} tBNB ({} gas at {} gwei)",
        units(fee, 18)?,
        tx.gas_limit,
        units(U256::from(tx.max_fee_per_gas), 9)?
    );
    println!("  hash       {}", envelope.tx_hash());
    println!();
    Ok(())
}

fn fund(needed: U256) -> Result<()> {
    println!(
        "need more than {} tBNB, get some from {FAUCET}",
        units(needed, 18)?
    );
    Ok(())
}

async fn wait(rpc: &Rpc, hash: &str) -> Result<Value> {
    for _ in 0..RECEIPT_TRIES {
        tokio::time::sleep(Duration::from_secs(1)).await;
        let receipt = rpc.call("eth_getTransactionReceipt", json!([hash])).await?;
        if !receipt.is_null() {
            return Ok(receipt);
        }
    }
    bail!("no receipt after {RECEIPT_TRIES} s")
}

fn quantity(value: &Value) -> Result<u128> {
    let digits = hex_digits(value)?;
    Ok(u128::from_str_radix(digits, 16)?)
}

fn big(value: &Value) -> Result<U256> {
    let digits = hex_digits(value)?;
    Ok(U256::from_str_radix(digits, 16)?)
}

fn hex_digits(value: &Value) -> Result<&str> {
    let text = value.as_str().context("expected a hex string")?;
    let digits = text.strip_prefix("0x").context("hex string without 0x")?;
    ensure!(!digits.is_empty(), "empty hex string");
    Ok(digits)
}

fn units(value: U256, decimals: u8) -> Result<String> {
    let text = format_units(value, decimals)?;
    Ok(text.trim_end_matches('0').trim_end_matches('.').to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    const HARDHAT: &str = "test test test test test test test test test test test junk";

    #[test]
    fn signed_bytes_decode_to_the_same_transfer() {
        let signer = keys::evm(HARDHAT).unwrap();
        let from = signer.address();
        let tx = TxEip1559 {
            chain_id: CHAIN_ID,
            nonce: 7,
            gas_limit: 21_000,
            max_fee_per_gas: 100_000_000,
            max_priority_fee_per_gas: 100_000_000,
            to: TxKind::Call(from),
            value: U256::from(AMOUNT_WEI),
            ..TxEip1559::default()
        };
        let raw = sign(&signer, tx.clone()).unwrap();
        let decoded = TxEnvelope::decode_2718(&mut raw.as_slice()).unwrap();
        assert_eq!(decoded.recover_signer().unwrap(), from);
        assert_eq!(decoded.as_eip1559().unwrap().tx(), &tx);
    }
}
