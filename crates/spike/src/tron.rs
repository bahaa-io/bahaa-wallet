mod proto;

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use alloy_primitives::{B256, Signature, hex};
use alloy_signer::SignerSync;
use alloy_signer_local::PrivateKeySigner;
use anyhow::{Context, Result, bail, ensure};
use prost::Message;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::keys;
use crate::prompt::confirm;
use proto::{Any, Contract, Raw, Transaction, TransferContract};

const API: &str = "https://nile.trongrid.io";
const EXPLORER: &str = "https://nile.tronscan.org/#/transaction/";
const FAUCET: &str = "https://nileex.io/join/getJoinPage";
const ADDRESS_PREFIX: u8 = 0x41;
const TRANSFER_TYPE: i32 = 1;
const TRANSFER_URL: &str = "type.googleapis.com/protocol.TransferContract";
const AMOUNT_SUN: u64 = 1_000_000;
const SUN_PER_TRX: u64 = 1_000_000;
const EXPIRY_MS: i64 = 60_000;
const RESULT_BYTES: u64 = 64;
const GAP: Duration = Duration::from_millis(350);
const CONFIRM_TRIES: u32 = 60;

struct Node {
    http: reqwest::Client,
}

impl Node {
    fn new() -> Result<Self> {
        let http = reqwest::Client::builder()
            .user_agent(concat!("bahaa-spike/", env!("CARGO_PKG_VERSION")))
            .timeout(Duration::from_secs(15))
            .build()?;
        Ok(Self { http })
    }

    async fn post(&self, path: &str, body: &Value) -> Result<Value> {
        tokio::time::sleep(GAP).await;
        let reply: Value = self
            .http
            .post(format!("{API}{path}"))
            .json(body)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        if let Some(error) = reply.get("Error") {
            bail!("{path}: {error}");
        }
        Ok(reply)
    }
}

struct Prices {
    per_byte: u64,
    create_account: u64,
    activation: u64,
}

struct Bandwidth {
    free: u64,
    staked: u64,
}

struct Decoded {
    txid: B256,
    from: Vec<u8>,
    to: Vec<u8>,
    amount: u64,
    expiration: i64,
}

pub fn address(phrase: &str, index: u32) -> Result<String> {
    Ok(base58(&tron_bytes(&keys::tron(phrase, index)?)))
}

pub async fn transfer() -> Result<()> {
    let phrase = keys::load()?;
    let signer = keys::tron(&phrase, 0)?;
    let from = tron_bytes(&signer);
    let to = tron_bytes(&keys::tron(&phrase, 1)?);
    let node = Node::new()?;
    println!("tron nile    {}", base58(&from));
    println!("endpoint     {API}");
    let Some(balance) = account(&node, &from).await? else {
        println!("balance      0 TRX, account not activated yet");
        fund(AMOUNT_SUN);
        return Ok(());
    };
    println!("balance      {} TRX", trx(balance));
    let prices = prices(&node).await?;
    let worst = AMOUNT_SUN + prices.activation + prices.create_account;
    if balance < worst {
        fund(worst);
        return Ok(());
    }
    let new_recipient = account(&node, &to).await?.is_none();
    let bandwidth = bandwidth(&node, &from).await?;
    let raw = build(&node, &from, &to).await?;
    let (txid, bytes) = sign(&signer, raw)?;
    let decoded = decode(&bytes)?;
    ensure!(
        decoded.txid == txid,
        "decoded txid differs from the signed one"
    );
    let size = u64::try_from(bytes.len())? + RESULT_BYTES;
    let (fee, reason) = estimate(size, new_recipient, &bandwidth, &prices)?;
    review(&decoded, new_recipient, fee, &reason)?;
    if !confirm()? {
        println!("not sent");
        return Ok(());
    }
    let id = hex::encode(txid);
    let reply = node
        .post(
            "/wallet/broadcasthex",
            &json!({"transaction": hex::encode(&bytes)}),
        )
        .await?;
    if reply["result"] != true {
        bail!("broadcast refused: {} {}", reply["code"], message(&reply));
    }
    ensure!(
        reply["txid"].as_str() == Some(id.as_str()),
        "node answered with a different txid {}",
        reply["txid"]
    );
    println!("sent         {id}");
    let info = wait(&node, &id).await?;
    let status = status(&node, &id).await?;
    let paid = info["fee"].as_u64().unwrap_or(0);
    println!(
        "confirmed    block {}, {status}, fee paid {} TRX, estimated {} TRX",
        info["blockNumber"],
        trx(paid),
        trx(fee)
    );
    println!("explorer     {EXPLORER}{id}");
    Ok(())
}

async fn account(node: &Node, address: &[u8]) -> Result<Option<u64>> {
    let reply = node
        .post(
            "/wallet/getaccount",
            &json!({"address": hex::encode(address)}),
        )
        .await?;
    if reply.get("address").is_none() {
        return Ok(None);
    }
    Ok(Some(reply["balance"].as_u64().unwrap_or(0)))
}

async fn prices(node: &Node) -> Result<Prices> {
    let reply = node.post("/wallet/getchainparameters", &json!({})).await?;
    let list = reply["chainParameter"]
        .as_array()
        .context("no chain parameters")?;
    let find = |key: &str| -> Result<u64> {
        list.iter()
            .find(|entry| entry["key"] == key)
            .and_then(|entry| entry["value"].as_u64())
            .with_context(|| format!("no {key} parameter"))
    };
    Ok(Prices {
        per_byte: find("getTransactionFee")?,
        create_account: find("getCreateAccountFee")?,
        activation: find("getCreateNewAccountFeeInSystemContract")?,
    })
}

async fn bandwidth(node: &Node, address: &[u8]) -> Result<Bandwidth> {
    let reply = node
        .post(
            "/wallet/getaccountresource",
            &json!({"address": hex::encode(address)}),
        )
        .await?;
    let field = |key: &str| reply[key].as_u64().unwrap_or(0);
    Ok(Bandwidth {
        free: field("freeNetLimit").saturating_sub(field("freeNetUsed")),
        staked: field("NetLimit").saturating_sub(field("NetUsed")),
    })
}

async fn build(node: &Node, from: &[u8], to: &[u8]) -> Result<Raw> {
    let block = node.post("/wallet/getnowblock", &json!({})).await?;
    let id = hex::decode(block["blockID"].as_str().context("block without an id")?)?;
    ensure!(id.len() == 32, "block id has {} bytes", id.len());
    let header = &block["block_header"]["raw_data"];
    let number = header["number"]
        .as_u64()
        .context("block without a number")?;
    let time = header["timestamp"]
        .as_i64()
        .context("block without a timestamp")?;
    let transfer = TransferContract {
        owner_address: from.to_vec(),
        to_address: to.to_vec(),
        amount: i64::try_from(AMOUNT_SUN)?,
    };
    Ok(Raw {
        ref_block_bytes: number.to_be_bytes()[6..].to_vec(),
        ref_block_hash: id[8..16].to_vec(),
        expiration: time + EXPIRY_MS,
        contract: vec![Contract {
            kind: TRANSFER_TYPE,
            parameter: Some(Any {
                type_url: TRANSFER_URL.to_owned(),
                value: transfer.encode_to_vec(),
            }),
        }],
        timestamp: now_ms()?,
    })
}

fn sign(signer: &PrivateKeySigner, raw: Raw) -> Result<(B256, Vec<u8>)> {
    let txid = B256::from_slice(&Sha256::digest(raw.encode_to_vec()));
    let signature = signer.sign_hash_sync(&txid)?;
    let tx = Transaction {
        raw_data: Some(raw),
        signature: vec![signature.as_bytes().to_vec()],
    };
    Ok((txid, tx.encode_to_vec()))
}

fn decode(bytes: &[u8]) -> Result<Decoded> {
    let tx = Transaction::decode(bytes)?;
    ensure!(
        tx.encode_to_vec() == bytes,
        "transaction carries fields we do not understand"
    );
    let raw = tx
        .raw_data
        .as_ref()
        .context("transaction without raw data")?;
    ensure!(
        raw.contract.len() == 1,
        "{} contracts, expected one",
        raw.contract.len()
    );
    let contract = &raw.contract[0];
    ensure!(
        contract.kind == TRANSFER_TYPE,
        "contract type {}, expected a TRX transfer",
        contract.kind
    );
    let parameter = contract
        .parameter
        .as_ref()
        .context("contract without a parameter")?;
    ensure!(
        parameter.type_url == TRANSFER_URL,
        "unexpected parameter type {}",
        parameter.type_url
    );
    let transfer = TransferContract::decode(parameter.value.as_slice())?;
    ensure!(tx.signature.len() == 1, "expected exactly one signature");
    let txid = B256::from_slice(&Sha256::digest(raw.encode_to_vec()));
    let signer = Signature::from_raw(&tx.signature[0])?.recover_address_from_prehash(&txid)?;
    let mut owner = vec![ADDRESS_PREFIX];
    owner.extend_from_slice(signer.as_slice());
    ensure!(
        owner == transfer.owner_address,
        "signature does not belong to the sender"
    );
    Ok(Decoded {
        txid,
        from: transfer.owner_address,
        to: transfer.to_address,
        amount: u64::try_from(transfer.amount)?,
        expiration: raw.expiration,
    })
}

fn estimate(
    bytes: u64,
    new_recipient: bool,
    bandwidth: &Bandwidth,
    prices: &Prices,
) -> Result<(u64, String)> {
    if new_recipient {
        let creation = if bandwidth.staked >= bytes {
            0
        } else {
            prices.create_account
        };
        return Ok((
            prices.activation + creation,
            format!(
                "activates the recipient: {} TRX activation, {} TRX bandwidth",
                trx(prices.activation),
                trx(creation)
            ),
        ));
    }
    if bandwidth.staked >= bytes {
        return Ok((0, format!("{bytes} bytes of staked bandwidth")));
    }
    if bandwidth.free >= bytes {
        return Ok((0, format!("{bytes} bytes of the free daily bandwidth")));
    }
    let burn = bytes.checked_mul(prices.per_byte).context("fee overflow")?;
    Ok((
        burn,
        format!("{bytes} bytes at {} sun per byte", prices.per_byte),
    ))
}

fn review(decoded: &Decoded, new_recipient: bool, fee: u64, reason: &str) -> Result<()> {
    let expires = (decoded.expiration - now_ms()?) / 1000;
    let recipient = if new_recipient {
        "not activated yet"
    } else {
        "active"
    };
    println!();
    println!("review, decoded from the signed bytes");
    println!("  network    Tron Nile testnet");
    println!("  from       {}", base58(&decoded.from));
    println!("  to         {} ({recipient})", base58(&decoded.to));
    println!("  amount     {} TRX", trx(decoded.amount));
    println!("  fee        {} TRX, {reason}", trx(fee));
    println!("  expires    in {expires} s");
    println!("  txid       {}", hex::encode(decoded.txid));
    println!();
    Ok(())
}

async fn wait(node: &Node, id: &str) -> Result<Value> {
    for _ in 0..CONFIRM_TRIES {
        tokio::time::sleep(Duration::from_secs(1)).await;
        let info = node
            .post("/wallet/gettransactioninfobyid", &json!({"value": id}))
            .await?;
        if info.get("blockNumber").is_some() {
            return Ok(info);
        }
    }
    bail!("not in a block after {CONFIRM_TRIES} s")
}

async fn status(node: &Node, id: &str) -> Result<String> {
    let tx = node
        .post("/wallet/gettransactionbyid", &json!({"value": id}))
        .await?;
    Ok(tx["ret"][0]["contractRet"]
        .as_str()
        .unwrap_or("unknown")
        .to_lowercase())
}

fn fund(needed: u64) {
    println!(
        "need at least {} TRX, get test TRX from {FAUCET}",
        trx(needed)
    );
}

fn message(reply: &Value) -> String {
    let text = reply["message"].as_str().unwrap_or_default();
    hex::decode(text).map_or_else(
        |_| text.to_owned(),
        |bytes| String::from_utf8_lossy(&bytes).into_owned(),
    )
}

fn tron_bytes(signer: &PrivateKeySigner) -> Vec<u8> {
    let mut bytes = vec![ADDRESS_PREFIX];
    bytes.extend_from_slice(signer.address().as_slice());
    bytes
}

fn base58(bytes: &[u8]) -> String {
    bs58::encode(bytes).with_check().into_string()
}

fn trx(sun: u64) -> String {
    let whole = sun / SUN_PER_TRX;
    let fraction = sun % SUN_PER_TRX;
    if fraction == 0 {
        whole.to_string()
    } else {
        format!("{whole}.{fraction:06}")
            .trim_end_matches('0')
            .to_owned()
    }
}

fn now_ms() -> Result<i64> {
    Ok(i64::try_from(
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis(),
    )?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base58_matches_known_tron_addresses() {
        let usdt = hex::decode("41a614f803b6fd780986a42c78ec9c7f77e6ded13c").unwrap();
        assert_eq!(base58(&usdt), "TR7NHqjeKQxGTCi8q8ZY4pL8otSzgjLj6t");
        let mut black_hole = vec![ADDRESS_PREFIX];
        black_hole.extend_from_slice(&[0; 20]);
        assert_eq!(base58(&black_hole), "T9yD14Nj9j7xAB4dbGeiX9h8unkKHxuWwb");
    }

    #[test]
    fn signed_bytes_decode_to_the_same_transfer() {
        let phrase = "test test test test test test test test test test test junk";
        let signer = keys::tron(phrase, 0).unwrap();
        let from = tron_bytes(&signer);
        let to = tron_bytes(&keys::tron(phrase, 1).unwrap());
        let transfer = TransferContract {
            owner_address: from.clone(),
            to_address: to.clone(),
            amount: 1_000_000,
        };
        let raw = Raw {
            ref_block_bytes: vec![0x12, 0x34],
            ref_block_hash: vec![7; 8],
            expiration: 1_800_000_060_000,
            contract: vec![Contract {
                kind: TRANSFER_TYPE,
                parameter: Some(Any {
                    type_url: TRANSFER_URL.to_owned(),
                    value: transfer.encode_to_vec(),
                }),
            }],
            timestamp: 1_800_000_000_000,
        };
        let (txid, bytes) = sign(&signer, raw).unwrap();
        let decoded = decode(&bytes).unwrap();
        assert_eq!(decoded.txid, txid);
        assert_eq!(decoded.from, from);
        assert_eq!(decoded.to, to);
        assert_eq!(decoded.amount, 1_000_000);
    }

    #[test]
    fn tampered_bytes_are_rejected() {
        let phrase = "test test test test test test test test test test test junk";
        let signer = keys::tron(phrase, 0).unwrap();
        let transfer = TransferContract {
            owner_address: tron_bytes(&signer),
            to_address: tron_bytes(&keys::tron(phrase, 1).unwrap()),
            amount: 1_000_000,
        };
        let raw = Raw {
            contract: vec![Contract {
                kind: TRANSFER_TYPE,
                parameter: Some(Any {
                    type_url: TRANSFER_URL.to_owned(),
                    value: transfer.encode_to_vec(),
                }),
            }],
            ..Raw::default()
        };
        let (_, mut bytes) = sign(&signer, raw).unwrap();
        let last = bytes.len() - 70;
        bytes[last] ^= 1;
        assert!(decode(&bytes).is_err());
    }
}
