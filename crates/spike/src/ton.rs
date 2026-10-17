use std::time::{Duration, SystemTime, UNIX_EPOCH};

use alloy_primitives::hex;
use anyhow::{Context, Result, bail, ensure};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use num_bigint::BigUint;
use reqwest::StatusCode;
use serde_json::{Value, json};
use tonlib_core::TonAddress;
use tonlib_core::cell::{ArcCell, BagOfCells, Cell, CellBuilder};
use tonlib_core::tlb_types::block::coins::{CurrencyCollection, Grams};
use tonlib_core::tlb_types::block::message::{CommonMsgInfo, IntMsgInfo, Message};
use tonlib_core::tlb_types::block::msg_address::MsgAddrNone;
use tonlib_core::tlb_types::tlb::TLB;
use tonlib_core::wallet::mnemonic::KeyPair;
use tonlib_core::wallet::ton_wallet::TonWallet;
use tonlib_core::wallet::version_helper::VersionHelper;
use tonlib_core::wallet::versioned::DEFAULT_WALLET_ID_V5R1_TESTNET;
use tonlib_core::wallet::versioned::v5::WalletExtMsgBodyV5;
use tonlib_core::wallet::wallet_version::WalletVersion;

use crate::keys;
use crate::prompt::confirm;

const API: &str = "https://testnet.toncenter.com/api";
const EXPLORER: &str = "https://testnet.tonviewer.com/transaction/";
const FAUCET: &str = "https://t.me/testgiver_ton_bot";
const VERSION: WalletVersion = WalletVersion::V5R1;
const AMOUNT_NANO: u64 = 10_000_000;
const RESERVE_NANO: u64 = 50_000_000;
const NANO_PER_TON: u64 = 1_000_000_000;
const VALID_FOR: u64 = 120;
const GAP: Duration = Duration::from_millis(1100);
const CONFIRM_TRIES: u32 = 40;
const RETRIES: u32 = 5;
const SCAN: u32 = 5;

struct Center {
    http: reqwest::Client,
}

impl Center {
    fn new() -> Result<Self> {
        let http = reqwest::Client::builder()
            .user_agent(concat!("bahaa-spike/", env!("CARGO_PKG_VERSION")))
            .timeout(Duration::from_secs(20))
            .build()?;
        Ok(Self { http })
    }

    async fn v2_get(&self, path: &str) -> Result<Value> {
        let reply = self
            .send(|| self.http.get(format!("{API}/v2{path}")))
            .await?;
        unwrap(&reply, path)
    }

    async fn v2_post(&self, path: &str, body: &Value) -> Result<Value> {
        let reply = self
            .send(|| self.http.post(format!("{API}/v2{path}")).json(body))
            .await?;
        unwrap(&reply, path)
    }

    async fn v3_get(&self, path: &str) -> Result<Value> {
        self.send(|| self.http.get(format!("{API}/v3{path}"))).await
    }

    async fn send(&self, build: impl Fn() -> reqwest::RequestBuilder) -> Result<Value> {
        for attempt in 1..=RETRIES {
            tokio::time::sleep(GAP * attempt).await;
            let response = build().send().await?;
            if response.status() != StatusCode::TOO_MANY_REQUESTS {
                return Ok(response.json().await?);
            }
        }
        bail!("still rate limited after {RETRIES} tries")
    }
}

struct Decoded {
    wallet: TonAddress,
    to: TonAddress,
    amount: u64,
    bounce: bool,
    mode: u8,
    seqno: u32,
    valid_until: u32,
    deploys: bool,
}

pub fn address(phrase: &str) -> Result<String> {
    Ok(friendly(&wallet(&keys::ton(phrase)?)?.address))
}

pub async fn transfer() -> Result<()> {
    let phrase = keys::load()?;
    let key = keys::ton(&phrase)?;
    let wallet = wallet(&key)?;
    let address = friendly(&wallet.address);
    let center = Center::new()?;
    println!("ton testnet  {address}");
    println!("endpoint     {API}");
    let info = center
        .v2_get(&format!("/getWalletInformation?address={address}"))
        .await?;
    let balance: u64 = info["balance"]
        .as_str()
        .context("no balance in the reply")?
        .parse()?;
    let state = info["account_state"].as_str().unwrap_or("unknown");
    let seqno = info["seqno"].as_u64().map_or(Ok(0), u32::try_from)?;
    println!("balance      {} TON, wallet {state}", ton(balance));
    if balance < AMOUNT_NANO + RESERVE_NANO {
        println!(
            "need at least {} TON, get test TON from {FAUCET}",
            ton(AMOUNT_NANO + RESERVE_NANO)
        );
        return Ok(());
    }
    let valid_until = u32::try_from(now()? + VALID_FOR)?;
    let boc = sign(&key, &wallet, seqno, valid_until, state != "active")?;
    let decoded = decode(&boc, &key.verifying_key())?;
    ensure!(
        decoded.wallet == wallet.address,
        "message is addressed to another wallet"
    );
    let fee = estimate(&center, &address, &boc).await?;
    review(&decoded, fee)?;
    if !confirm()? {
        println!("not sent");
        return Ok(());
    }
    let sent = center
        .v2_post("/sendBocReturnHash", &json!({"boc": STANDARD.encode(&boc)}))
        .await?;
    let message = sent["hash"]
        .as_str()
        .context("send reply without a message hash")?;
    println!("sent         message {message}");
    let tx = wait(&center, &address, message).await?;
    let paid: u64 = tx["total_fees"].as_str().unwrap_or("0").parse()?;
    let status = if tx["description"]["aborted"] == false {
        "success"
    } else {
        "aborted"
    };
    let hash = hex::encode(STANDARD.decode(tx["hash"].as_str().context("no tx hash")?)?);
    println!(
        "confirmed    {status}, transaction fee {} TON, total sending cost {} TON",
        ton(paid),
        ton(fee)
    );
    println!("explorer     {EXPLORER}{hash}");
    Ok(())
}

fn wallet(key: &SigningKey) -> Result<TonWallet> {
    let public = KeyPair {
        public_key: key.verifying_key().to_bytes().to_vec(),
        secret_key: Vec::new(),
    };
    Ok(TonWallet::new_with_params(
        VERSION,
        public,
        0,
        DEFAULT_WALLET_ID_V5R1_TESTNET,
    )?)
}

fn internal(dest: &TonAddress, amount: u64) -> Result<ArcCell> {
    let info = CommonMsgInfo::Int(IntMsgInfo {
        ihr_disabled: true,
        bounce: false,
        bounced: false,
        src: MsgAddrNone {}.into(),
        dest: dest.to_msg_address_int().into(),
        value: CurrencyCollection::new(BigUint::from(amount)),
        ihr_fee: Grams::new(BigUint::default()),
        fwd_fee: Grams::new(BigUint::default()),
        created_lt: 0,
        created_at: 0,
    });
    let body = CellBuilder::new().build()?.to_arc();
    Ok(Message::new(info, body).to_cell()?.to_arc())
}

fn sign(
    key: &SigningKey,
    wallet: &TonWallet,
    seqno: u32,
    valid_until: u32,
    deploy: bool,
) -> Result<Vec<u8>> {
    let transfer = internal(&wallet.address, AMOUNT_NANO)?;
    let body = wallet.create_external_body(valid_until, seqno, [transfer])?;
    let signature = key.sign(body.cell_hash().as_slice()).to_bytes();
    let signed = VersionHelper::sign_msg(VERSION, &body, &signature)?;
    let external = wallet.wrap_signed_body(signed, deploy)?;
    Ok(BagOfCells::from_root(external).serialize(true)?)
}

fn decode(boc: &[u8], key: &VerifyingKey) -> Result<Decoded> {
    let root = BagOfCells::parse(boc)?.single_root()?;
    let message = Message::from_cell(&root)?;
    ensure!(
        message.to_cell()?.cell_hash() == Cell::cell_hash(&root),
        "message carries data we do not understand"
    );
    let CommonMsgInfo::ExtIn(info) = &message.info else {
        bail!("not an external message");
    };
    let body = &message.body.value;
    let mut parser = body.parser();
    let request = WalletExtMsgBodyV5::read(&mut parser)?;
    let signature = Signature::from_slice(&parser.load_bytes(64)?)?;
    parser.ensure_empty()?;
    let unsigned = request.to_cell()?;
    key.verify_strict(unsigned.cell_hash().as_slice(), &signature)?;
    ensure!(
        VersionHelper::sign_msg(VERSION, &unsigned, &signature.to_bytes())?.cell_hash()
            == Cell::cell_hash(body),
        "wallet body carries data we do not understand"
    );
    ensure!(
        request.wallet_id == DEFAULT_WALLET_ID_V5R1_TESTNET,
        "signed for wallet id {:#x}, not the testnet one",
        request.wallet_id
    );
    ensure!(request.msgs.len() == 1, "expected exactly one transfer");
    let out = Message::from_cell(&request.msgs[0])?;
    let CommonMsgInfo::Int(transfer) = &out.info else {
        bail!("the wallet would send an external message");
    };
    Ok(Decoded {
        wallet: TonAddress::try_from(info.dest.clone())?,
        to: TonAddress::try_from(transfer.dest.clone())?,
        amount: u64::try_from(&transfer.value.grams.amount)?,
        bounce: transfer.bounce,
        mode: request.msgs_modes[0],
        seqno: request.msg_seqno,
        valid_until: request.valid_until,
        deploys: message.init.is_some(),
    })
}

async fn estimate(center: &Center, address: &str, boc: &[u8]) -> Result<u64> {
    let root = BagOfCells::parse(boc)?.single_root()?;
    let message = Message::from_cell(&root)?;
    let body = BagOfCells::new(std::slice::from_ref(&message.body.value)).serialize(true)?;
    let (code, data) = match &message.init {
        Some(init) => (
            part(init.value.code.as_ref().map(|code| &code.0))?,
            part(init.value.data.as_ref().map(|data| &data.0))?,
        ),
        None => (String::new(), String::new()),
    };
    let reply = center
        .v2_post(
            "/estimateFee",
            &json!({
                "address": address,
                "body": STANDARD.encode(body),
                "init_code": code,
                "init_data": data,
                "ignore_chksig": true,
            }),
        )
        .await?;
    let fees = &reply["source_fees"];
    ["in_fwd_fee", "storage_fee", "gas_fee", "fwd_fee"]
        .iter()
        .try_fold(0u64, |sum, key| {
            let fee = fees[*key]
                .as_u64()
                .with_context(|| format!("no {key} in the fee estimate"))?;
            sum.checked_add(fee).context("fee overflow")
        })
}

fn part(cell: Option<&ArcCell>) -> Result<String> {
    let cell = cell.context("state init without code or data")?;
    Ok(STANDARD.encode(BagOfCells::new(std::slice::from_ref(cell)).serialize(true)?))
}

fn review(decoded: &Decoded, fee: u64) -> Result<()> {
    let expires = u64::from(decoded.valid_until).saturating_sub(now()?);
    println!();
    println!("review, decoded from the signed bytes");
    println!("  network    TON testnet, wallet v5r1");
    println!("  from       {}", friendly(&decoded.wallet));
    println!(
        "  to         {} ({})",
        friendly(&decoded.to),
        if decoded.bounce {
            "bounceable"
        } else {
            "non-bounceable"
        }
    );
    println!("  amount     {} TON", ton(decoded.amount));
    println!("  fee        {} TON estimated", ton(fee));
    println!(
        "  seqno      {}{}",
        decoded.seqno,
        if decoded.deploys {
            ", deploys the wallet contract"
        } else {
            ""
        }
    );
    println!("  send mode  {}", decoded.mode);
    println!("  expires    in {expires} s");
    println!();
    Ok(())
}

async fn wait(center: &Center, address: &str, message: &str) -> Result<Value> {
    for _ in 0..CONFIRM_TRIES {
        let list = center
            .v3_get(&format!(
                "/transactions?account={address}&limit={SCAN}&sort=desc"
            ))
            .await?;
        let found = list["transactions"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|tx| tx["in_msg"]["hash"] == message);
        if let Some(tx) = found {
            return Ok(tx.clone());
        }
    }
    bail!("message {message} is not in a transaction after {CONFIRM_TRIES} checks")
}

fn unwrap(reply: &Value, path: &str) -> Result<Value> {
    if reply["ok"] != true {
        let reason = match &reply["error"] {
            Value::Null => &reply["result"],
            error => error,
        };
        bail!("{path}: {} {reason}", reply["code"]);
    }
    reply
        .get("result")
        .cloned()
        .with_context(|| format!("{path}: reply without a result"))
}

fn friendly(address: &TonAddress) -> String {
    address.to_base64_url_flags(true, true)
}

fn ton(nano: u64) -> String {
    let whole = nano / NANO_PER_TON;
    let fraction = nano % NANO_PER_TON;
    if fraction == 0 {
        whole.to_string()
    } else {
        format!("{whole}.{fraction:09}")
            .trim_end_matches('0')
            .to_owned()
    }
}

fn now() -> Result<u64> {
    Ok(SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;

    const PHRASE: &str = "test test test test test test test test test test test junk";

    #[test]
    fn signed_message_decodes_to_the_same_transfer() {
        let key = keys::ton(PHRASE).unwrap();
        let wallet = wallet(&key).unwrap();
        let boc = sign(&key, &wallet, 0, 1_800_000_000, true).unwrap();
        let decoded = decode(&boc, &key.verifying_key()).unwrap();
        assert_eq!(decoded.wallet, wallet.address);
        assert_eq!(decoded.to, wallet.address);
        assert_eq!(decoded.amount, AMOUNT_NANO);
        assert_eq!(decoded.seqno, 0);
        assert_eq!(decoded.valid_until, 1_800_000_000);
        assert!(decoded.deploys);
        assert!(!decoded.bounce);
    }

    #[test]
    fn a_different_key_fails_verification() {
        let key = keys::ton(PHRASE).unwrap();
        let wallet = wallet(&key).unwrap();
        let boc = sign(&key, &wallet, 3, 1_800_000_000, false).unwrap();
        let stranger = SigningKey::from_bytes(&[9; 32]).verifying_key();
        assert!(decode(&boc, &stranger).is_err());
    }
}
