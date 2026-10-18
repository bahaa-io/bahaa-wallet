use bahaa_chain::{ChainError, Incoming, ScanCursor, Scanned, TxId};
use bahaa_config::Feature;
use bahaa_types::{Address, AssetId};
use ruint::aliases::U256;
use serde_json::{Value, json};

use super::{TronChain, amount, bytes, malformed, now_ms};
use crate::http::{hex, node_text};
use crate::tx;

const TRANSFER_TOPIC: &str = "ddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef";
const REACH_MS: u64 = 30 * 24 * 60 * 60 * 1000;
const PAGE: u32 = 200;
const PAGES: usize = 5;
const SETTLE_MS: u64 = 5 * 60 * 1000;
const MAX_FINGERPRINT: usize = 2048;

struct Listing {
    entries: Vec<Value>,
    next: u64,
}

impl TronChain {
    pub fn can_scan(&self) -> bool {
        self.network
            .endpoints
            .iter()
            .any(|endpoint| endpoint.has(Feature::History))
    }

    pub async fn scan(
        &self,
        to: &Address,
        assets: &[AssetId],
        cursor: Option<ScanCursor>,
    ) -> Result<Option<Scanned>, ChainError> {
        let owner = bytes(to)?;
        let now = u64::try_from(now_ms()).unwrap_or(0);
        let start = cursor.map_or(now.saturating_sub(REACH_MS), |cursor| cursor.next);
        let mut found = Vec::new();
        let mut next: Option<u64> = None;
        for asset in assets
            .iter()
            .filter(|asset| asset.network() == &self.network.id)
        {
            let (entries, reached) = match asset.contract() {
                None => self.coins_to(&owner, start).await?,
                Some(contract) => {
                    self.tokens_to(&owner, asset, &bytes(contract)?, start)
                        .await?
                }
            };
            found.extend(entries);
            next = Some(next.map_or(reached, |next| next.min(reached)));
        }
        let Some(next) = next else {
            return Ok(None);
        };
        if cursor.is_some_and(|cursor| cursor.next == next) && found.is_empty() {
            return Ok(None);
        }
        let complete_from = cursor.map_or(start, |cursor| cursor.complete_from);
        Ok(Some(Scanned {
            cursor: ScanCursor {
                next,
                complete_from,
                complete_since: complete_from / 1000,
            },
            found,
            sent: Vec::new(),
        }))
    }

    async fn coins_to(
        &self,
        owner: &[u8; 20],
        start: u64,
    ) -> Result<(Vec<Incoming>, u64), ChainError> {
        let path = format!("v1/accounts/{}/transactions", Address::Tron(*owner));
        let listing = self.listing(&path, None, start).await?;
        let found = listing
            .entries
            .iter()
            .filter_map(|entry| self.coin_entry(entry, owner))
            .collect();
        Ok((found, listing.next))
    }

    fn coin_entry(&self, entry: &Value, owner: &[u8; 20]) -> Option<Incoming> {
        if entry["raw_data"]["contract"][0]["type"] != "TransferContract"
            || entry["ret"][0]["contractRet"] != "SUCCESS"
        {
            return None;
        }
        let raw = crate::http::unhex(entry["raw_data_hex"].as_str()?)?;
        let coins = tx::coins_in(&raw)?;
        let listed = entry["txID"]
            .as_str()
            .is_some_and(|text| text.eq_ignore_ascii_case(&hex(&coins.id)));
        if coins.to != *owner || !listed {
            return None;
        }
        Some(Incoming {
            id: TxId::new(hex(&coins.id)).ok()?,
            index: 0,
            from: Address::Tron(coins.from),
            to: Address::Tron(coins.to),
            asset: AssetId::Native(self.network.id.clone()),
            amount: amount(U256::from(coins.sun), self.network.native.decimals).ok()?,
            block: entry["blockNumber"].as_u64()?,
            time: entry["block_timestamp"].as_u64()? / 1000,
        })
    }

    async fn tokens_to(
        &self,
        owner: &[u8; 20],
        asset: &AssetId,
        contract: &[u8; 20],
        start: u64,
    ) -> Result<(Vec<Incoming>, u64), ChainError> {
        let path = format!("v1/accounts/{}/transactions/trc20", Address::Tron(*owner));
        let filter = ("contract_address", Address::Tron(*contract).to_string());
        let listing = self.listing(&path, Some(filter), start).await?;
        let mut ids: Vec<String> = Vec::new();
        for entry in &listing.entries {
            let id = entry["transaction_id"]
                .as_str()
                .filter(|id| id.len() == 64 && id.bytes().all(|byte| byte.is_ascii_hexdigit()))
                .ok_or_else(malformed)?
                .to_ascii_lowercase();
            if !ids.contains(&id) {
                ids.push(id);
            }
        }
        let mut found = Vec::new();
        if ids.is_empty() {
            return Ok((found, listing.next));
        }
        let decimals = self.token_decimals(owner, contract).await?;
        for id in ids {
            let info = self
                .http
                .post("wallet/gettransactioninfobyid", json!({"value": id}))
                .await?;
            for (index, from, units) in transfers_to(&info, &id, contract, owner)? {
                found.push(Incoming {
                    id: TxId::new(id.clone())?,
                    index,
                    from: Address::Tron(from),
                    to: Address::Tron(*owner),
                    asset: asset.clone(),
                    amount: amount(units, decimals)?,
                    block: info["blockNumber"].as_u64().ok_or_else(malformed)?,
                    time: info["blockTimeStamp"].as_u64().ok_or_else(malformed)? / 1000,
                });
            }
        }
        Ok((found, listing.next))
    }

    async fn listing(
        &self,
        path: &str,
        filter: Option<(&str, String)>,
        start: u64,
    ) -> Result<Listing, ChainError> {
        let mut entries = Vec::new();
        let mut fingerprint: Option<String> = None;
        for _ in 0..PAGES {
            let mut query = vec![
                ("only_to", "true".to_owned()),
                ("only_confirmed", "true".to_owned()),
                ("order_by", "block_timestamp,asc".to_owned()),
                ("min_timestamp", start.to_string()),
                ("limit", PAGE.to_string()),
            ];
            query.extend(filter.clone());
            query.extend(fingerprint.take().map(|value| ("fingerprint", value)));
            let reply = self.http.history(path, &query).await?;
            if reply["success"] != true {
                return Err(ChainError::Network(format!(
                    "the history service refused: {}",
                    node_text(&reply["error"])
                )));
            }
            let page = reply["data"].as_array().ok_or_else(malformed)?;
            entries.extend(page.iter().cloned());
            fingerprint = reply["meta"]["fingerprint"]
                .as_str()
                .filter(|value| !page.is_empty() && fingerprint_ok(value))
                .map(str::to_owned);
            if fingerprint.is_none() {
                let after_last = latest(&entries)?.map_or(start, |last| last.saturating_add(1));
                let settled = reply["meta"]["at"]
                    .as_u64()
                    .map_or(0, |at| at.saturating_sub(SETTLE_MS));
                return Ok(Listing {
                    entries,
                    next: after_last.max(settled).max(start),
                });
            }
        }
        let last = latest(&entries)?.unwrap_or(start);
        let next = if last > start {
            last
        } else {
            start.saturating_add(1)
        };
        Ok(Listing { entries, next })
    }
}

fn latest(entries: &[Value]) -> Result<Option<u64>, ChainError> {
    entries.iter().try_fold(None, |latest: Option<u64>, entry| {
        let time = entry["block_timestamp"].as_u64().ok_or_else(malformed)?;
        Ok(Some(latest.map_or(time, |latest| latest.max(time))))
    })
}

fn fingerprint_ok(value: &str) -> bool {
    (1..=MAX_FINGERPRINT).contains(&value.len())
        && value.bytes().all(|byte| byte.is_ascii_alphanumeric())
}

fn transfers_to(
    info: &Value,
    id: &str,
    contract: &[u8; 20],
    owner: &[u8; 20],
) -> Result<Vec<(u64, [u8; 20], U256)>, ChainError> {
    let known = info.as_object().is_some_and(|info| !info.is_empty());
    if !known {
        return Err(ChainError::Network(
            "an endpoint does not know a listed transaction yet".to_owned(),
        ));
    }
    if !info["id"]
        .as_str()
        .is_some_and(|text| text.eq_ignore_ascii_case(id))
    {
        return Err(ChainError::Network(
            "an endpoint answered for another transaction".to_owned(),
        ));
    }
    if info["receipt"]["result"] != "SUCCESS" {
        return Ok(Vec::new());
    }
    let Some(logs) = info.get("log") else {
        return Ok(Vec::new());
    };
    let logs = logs.as_array().ok_or_else(malformed)?;
    let mut found = Vec::new();
    for (index, log) in logs.iter().enumerate() {
        if let Some((from, units)) = transfer_log(log, contract, owner) {
            found.push((u64::try_from(index).map_err(|_| malformed())?, from, units));
        }
    }
    Ok(found)
}

fn transfer_log(log: &Value, contract: &[u8; 20], owner: &[u8; 20]) -> Option<([u8; 20], U256)> {
    let emitter = log["address"].as_str()?;
    let emitter = if emitter.len() == 42 {
        emitter.strip_prefix("41")?
    } else {
        emitter
    };
    if !emitter.eq_ignore_ascii_case(&hex(contract)) {
        return None;
    }
    let topics = log["topics"].as_array()?;
    let to_us = format!("{:0>64}", hex(owner));
    let [kind, from, to] = topics.as_slice() else {
        return None;
    };
    if !same(kind, TRANSFER_TOPIC) || !same(to, &to_us) {
        return None;
    }
    let from = padded_address(from.as_str()?)?;
    let data = log["data"].as_str().filter(|data| digits(data, 64))?;
    let units = U256::from_str_radix(data, 16).ok()?;
    (!units.is_zero()).then_some((from, units))
}

fn same(value: &Value, expected: &str) -> bool {
    value
        .as_str()
        .is_some_and(|text| text.eq_ignore_ascii_case(expected))
}

fn digits(text: &str, len: usize) -> bool {
    text.len() == len && text.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn padded_address(word: &str) -> Option<[u8; 20]> {
    if !digits(word, 64) || !word[..24].bytes().all(|byte| byte == b'0') {
        return None;
    }
    let bytes = crate::http::unhex(&word[24..])?;
    bytes.try_into().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    const US: [u8; 20] = [0xa3; 20];
    const TOKEN: [u8; 20] = [0xa6; 20];
    const SENDER: [u8; 20] = [0x24; 20];

    fn log() -> Value {
        json!({
            "address": hex(&TOKEN),
            "topics": [TRANSFER_TOPIC, format!("{:0>64}", hex(&SENDER)), format!("{:0>64}", hex(&US))],
            "data": format!("{:064x}", 8_509_335)
        })
    }

    fn info(logs: &[Value]) -> Value {
        json!({
            "id": "ab".repeat(32),
            "blockNumber": 87_181_694,
            "blockTimeStamp": 1_792_218_060_000_u64,
            "receipt": {"result": "SUCCESS"},
            "log": logs
        })
    }

    #[test]
    fn reads_transfers_to_us_from_the_node_receipt() {
        let mut other = log();
        other["topics"][2] = json!(format!("{:0>64}", hex(&SENDER)));
        let mut prefixed = log();
        prefixed["address"] = json!(format!("41{}", hex(&TOKEN)));
        let found = transfers_to(
            &info(&[other, log(), prefixed]),
            &"AB".repeat(32),
            &TOKEN,
            &US,
        )
        .unwrap();
        assert_eq!(
            found,
            [
                (1, SENDER, U256::from(8_509_335u32)),
                (2, SENDER, U256::from(8_509_335u32))
            ]
        );
    }

    #[test]
    fn ignores_logs_that_are_not_exactly_a_transfer_of_this_token() {
        let edits: [(&str, Value); 6] = [
            ("address", json!(hex(&[0x11; 20]))),
            ("data", json!(format!("{:064x}", 0))),
            ("data", json!("01")),
            (
                "topics",
                json!([TRANSFER_TOPIC, format!("{:0>64}", hex(&SENDER))]),
            ),
            (
                "topics",
                json!([
                    "00".repeat(32),
                    format!("{:0>64}", hex(&SENDER)),
                    format!("{:0>64}", hex(&US))
                ]),
            ),
            (
                "topics",
                json!([
                    TRANSFER_TOPIC,
                    "11".repeat(32),
                    format!("{:0>64}", hex(&US))
                ]),
            ),
        ];
        for (field, value) in edits {
            let mut bad = log();
            bad[field] = value;
            assert_eq!(
                transfer_log(&bad, &TOKEN, &US),
                None,
                "{field}: {}",
                bad[field]
            );
        }
    }

    #[test]
    fn a_receipt_must_be_known_successful_and_about_this_transaction() {
        assert!(matches!(
            transfers_to(&json!({}), &"ab".repeat(32), &TOKEN, &US),
            Err(ChainError::Network(_))
        ));
        assert!(matches!(
            transfers_to(&info(&[log()]), &"cd".repeat(32), &TOKEN, &US),
            Err(ChainError::Network(_))
        ));
        let mut failed = info(&[log()]);
        failed["receipt"]["result"] = json!("REVERT");
        assert_eq!(
            transfers_to(&failed, &"ab".repeat(32), &TOKEN, &US),
            Ok(Vec::new())
        );
    }

    #[test]
    fn fingerprints_are_plain_tokens() {
        assert!(fingerprint_ok("2NgPQPX6egFQe6mpJFEk6T1gqGb7Fg4V"));
        for bad in ["", "a&b=c", "x/y", &"a".repeat(MAX_FINGERPRINT + 1)] {
            assert!(!fingerprint_ok(bad), "{bad}");
        }
    }
}
