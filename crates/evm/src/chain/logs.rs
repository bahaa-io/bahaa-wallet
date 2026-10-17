use std::collections::HashMap;

use bahaa_chain::{ChainError, TxId};
use bahaa_config::Feature;
use bahaa_types::{Address, Amount, AssetId};
use ruint::aliases::U256;
use serde_json::{Value, json};

use super::{EvmChain, amount, bytes, hex, word};
use crate::rpc::quantity;

const TRANSFER_TOPIC: &str = "0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef";
const CHUNK: u64 = 2_000;
const REACH: u64 = 4_000;
const CONFIRMATIONS: u64 = 12;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScanCursor {
    pub next: u64,
    pub complete_from: u64,
    pub complete_since: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Incoming {
    pub id: TxId,
    pub index: u64,
    pub from: Address,
    pub to: Address,
    pub asset: AssetId,
    pub amount: Amount,
    pub block: u64,
    pub time: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Scanned {
    pub cursor: ScanCursor,
    pub found: Vec<Incoming>,
}

#[derive(Debug, PartialEq, Eq)]
struct Range {
    from: u64,
    until: u64,
    reset: bool,
}

#[derive(Debug, PartialEq, Eq)]
struct Log {
    contract: [u8; 20],
    from: [u8; 20],
    units: U256,
    block: u64,
    id: String,
    index: u64,
    time: Option<u64>,
}

impl EvmChain {
    pub fn can_scan(&self) -> bool {
        self.network
            .endpoints
            .iter()
            .any(|endpoint| endpoint.has(Feature::Logs))
    }

    pub async fn scan(
        &self,
        to: &Address,
        tokens: &[AssetId],
        cursor: Option<ScanCursor>,
    ) -> Result<Option<Scanned>, ChainError> {
        let recipient = bytes(to)?;
        let head = block_number(&self.logs_call("eth_blockNumber", json!([])).await?)?;
        let Some(range) = plan(cursor, head) else {
            return Ok(None);
        };
        let tokens: Vec<(&AssetId, [u8; 20])> = tokens
            .iter()
            .filter(|asset| asset.network() == &self.network.id)
            .filter_map(|asset| Some((asset, bytes(asset.contract()?).ok()?)))
            .collect();
        let mut times = HashMap::new();
        let mut found = Vec::new();
        if !tokens.is_empty() {
            let mut from = range.from;
            while from <= range.until {
                let until = range.until.min(from.saturating_add(CHUNK - 1));
                for log in self.logs(&tokens, &recipient, from, until).await? {
                    let Some((asset, contract)) = tokens
                        .iter()
                        .find(|(_, contract)| *contract == log.contract)
                    else {
                        continue;
                    };
                    let decimals = self.token_decimals(contract).await?;
                    let time = match log.time {
                        Some(time) => time,
                        None => self.block_time(log.block, &mut times).await?,
                    };
                    found.push(Incoming {
                        id: TxId::new(log.id)?,
                        index: log.index,
                        from: Address::Evm(log.from),
                        to: *to,
                        asset: (*asset).clone(),
                        amount: amount(log.units, decimals)?,
                        block: log.block,
                        time,
                    });
                }
                from = until + 1;
            }
        }
        let (complete_from, complete_since) = match cursor {
            Some(cursor) if !range.reset => (cursor.complete_from, cursor.complete_since),
            _ => (range.from, self.block_time(range.from, &mut times).await?),
        };
        Ok(Some(Scanned {
            cursor: ScanCursor {
                next: range.until + 1,
                complete_from,
                complete_since,
            },
            found,
        }))
    }

    async fn logs(
        &self,
        tokens: &[(&AssetId, [u8; 20])],
        recipient: &[u8; 20],
        from: u64,
        until: u64,
    ) -> Result<Vec<Log>, ChainError> {
        let contracts: Vec<String> = tokens.iter().map(|(_, contract)| hex(contract)).collect();
        let filter = json!([{
            "address": contracts,
            "fromBlock": number(from),
            "toBlock": number(until),
            "topics": [TRANSFER_TOPIC, null, format!("0x{}", word(recipient))],
        }]);
        let reply = self.logs_call("eth_getLogs", filter).await?;
        let logs = reply.as_array().ok_or_else(malformed)?;
        Ok(logs
            .iter()
            .filter_map(|log| parse(log, recipient, from, until))
            .collect())
    }

    async fn block_time(
        &self,
        block: u64,
        cache: &mut HashMap<u64, u64>,
    ) -> Result<u64, ChainError> {
        if let Some(time) = cache.get(&block) {
            return Ok(*time);
        }
        let header = self
            .logs_call("eth_getBlockByNumber", json!([number(block), false]))
            .await?;
        let time = block_number(&header["timestamp"])?;
        cache.insert(block, time);
        Ok(time)
    }

    async fn logs_call(&self, method: &str, params: Value) -> Result<Value, ChainError> {
        self.rpc
            .call_with(Some(Feature::Logs), method, params)
            .await
    }
}

fn plan(cursor: Option<ScanCursor>, head: u64) -> Option<Range> {
    let until = head.checked_sub(CONFIRMATIONS)?;
    let earliest = until.saturating_sub(REACH - 1);
    match cursor {
        Some(cursor) if cursor.next > until => None,
        Some(cursor) if cursor.next >= earliest => Some(Range {
            from: cursor.next,
            until,
            reset: false,
        }),
        _ => Some(Range {
            from: earliest,
            until,
            reset: true,
        }),
    }
}

fn parse(log: &Value, recipient: &[u8; 20], from: u64, until: u64) -> Option<Log> {
    if log["removed"].as_bool() == Some(true) {
        return None;
    }
    let topics = log["topics"].as_array()?;
    let to_us = format!("0x{}", word(recipient));
    if topics.len() != 3 || !same(&topics[0], TRANSFER_TOPIC) || !same(&topics[2], &to_us) {
        return None;
    }
    let sender = padded_address(topics[1].as_str()?)?;
    let contract = decode20(digits(log["address"].as_str()?, 40)?)?;
    let units = U256::from_str_radix(digits(log["data"].as_str()?, 64)?, 16).ok()?;
    let block = block_number(&log["blockNumber"]).ok()?;
    if units.is_zero() || !(from..=until).contains(&block) {
        return None;
    }
    let id = log["transactionHash"].as_str()?.to_ascii_lowercase();
    digits(&id, 64)?;
    Some(Log {
        contract,
        from: sender,
        units,
        block,
        id,
        index: block_number(&log["logIndex"]).ok()?,
        time: block_number(&log["blockTimestamp"]).ok(),
    })
}

fn same(value: &Value, expected: &str) -> bool {
    value
        .as_str()
        .is_some_and(|text| text.eq_ignore_ascii_case(expected))
}

fn digits(text: &str, len: usize) -> Option<&str> {
    let digits = text.strip_prefix("0x")?;
    (digits.len() == len && digits.bytes().all(|byte| byte.is_ascii_hexdigit())).then_some(digits)
}

fn padded_address(text: &str) -> Option<[u8; 20]> {
    let digits = digits(text, 64)?;
    let (padding, address) = digits.split_at(24);
    padding.bytes().all(|byte| byte == b'0').then_some(())?;
    decode20(address)
}

fn decode20(digits: &str) -> Option<[u8; 20]> {
    let mut out = [0u8; 20];
    for (slot, at) in out.iter_mut().zip((0..digits.len()).step_by(2)) {
        *slot = u8::from_str_radix(digits.get(at..at + 2)?, 16).ok()?;
    }
    (digits.len() == 40).then_some(out)
}

fn block_number(value: &Value) -> Result<u64, ChainError> {
    u64::try_from(quantity(value)?).map_err(|_| malformed())
}

fn number(value: u64) -> String {
    format!("0x{value:x}")
}

fn malformed() -> ChainError {
    ChainError::Network("an endpoint sent a malformed log reply".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    const ME: [u8; 20] = [0xf3; 20];
    const TOKEN: [u8; 20] = [0x55; 20];
    const SENDER: [u8; 20] = [0x49; 20];

    fn cursor(next: u64) -> ScanCursor {
        ScanCursor {
            next,
            complete_from: 7,
            complete_since: 70,
        }
    }

    fn range(from: u64, until: u64, reset: bool) -> Range {
        Range { from, until, reset }
    }

    #[test]
    fn scans_forward_within_reach_and_waits_for_confirmations() {
        assert_eq!(plan(None, 10_000), Some(range(5_989, 9_988, true)));
        assert_eq!(
            plan(Some(cursor(9_000)), 10_000),
            Some(range(9_000, 9_988, false))
        );
        assert_eq!(
            plan(Some(cursor(5_989)), 10_000),
            Some(range(5_989, 9_988, false))
        );
        assert_eq!(
            plan(Some(cursor(5_988)), 10_000),
            Some(range(5_989, 9_988, true))
        );
        assert_eq!(plan(Some(cursor(9_989)), 10_000), None);
        assert_eq!(plan(None, 11), None);
        assert_eq!(plan(None, 100), Some(range(0, 88, true)));
    }

    fn log() -> Value {
        json!({
            "address": hex(&TOKEN),
            "topics": [TRANSFER_TOPIC, format!("0x{}", word(&SENDER)), format!("0x{}", word(&ME))],
            "data": format!("0x{:064x}", 1_500_000u64),
            "blockNumber": "0x64",
            "blockTimestamp": "0x6ad3e83e",
            "transactionHash": format!("0x{}", "AB".repeat(32)),
            "logIndex": "0x5",
            "removed": false
        })
    }

    #[test]
    fn reads_a_plain_token_transfer_to_us() {
        let parsed = parse(&log(), &ME, 100, 100).unwrap();
        assert_eq!(parsed.contract, TOKEN);
        assert_eq!(parsed.from, SENDER);
        assert_eq!(parsed.units, U256::from(1_500_000u64));
        assert_eq!(
            (parsed.block, parsed.index, parsed.time),
            (100, 5, Some(0x6ad3_e83e))
        );
        assert_eq!(parsed.id, format!("0x{}", "ab".repeat(32)));
        let mut untimed = log();
        untimed.as_object_mut().unwrap().remove("blockTimestamp");
        assert_eq!(parse(&untimed, &ME, 0, 200).unwrap().time, None);
    }

    #[test]
    fn ignores_anything_that_is_not_exactly_that() {
        let edits: [(&str, Value); 10] = [
            ("removed", json!(true)),
            ("data", json!(format!("0x{:064x}", 0))),
            ("data", json!("0x01")),
            ("blockNumber", json!("0x65")),
            ("transactionHash", json!("0xabc")),
            ("address", json!("0x55")),
            (
                "topics",
                json!([TRANSFER_TOPIC, format!("0x{}", word(&SENDER))]),
            ),
            (
                "topics",
                json!([
                    TRANSFER_TOPIC,
                    format!("0x{}", word(&SENDER)),
                    format!("0x{}", word(&SENDER))
                ]),
            ),
            (
                "topics",
                json!([
                    format!("0x{}", "00".repeat(32)),
                    format!("0x{}", word(&SENDER)),
                    format!("0x{}", word(&ME))
                ]),
            ),
            (
                "topics",
                json!([
                    TRANSFER_TOPIC,
                    format!("0x{}", "11".repeat(32)),
                    format!("0x{}", word(&ME))
                ]),
            ),
        ];
        for (field, value) in edits {
            let mut bad = log();
            bad[field] = value;
            assert_eq!(parse(&bad, &ME, 100, 100), None, "{field}: {}", bad[field]);
        }
        let mut nft = log();
        nft["topics"]
            .as_array_mut()
            .unwrap()
            .push(json!(format!("0x{:064x}", 1)));
        assert_eq!(parse(&nft, &ME, 100, 100), None);
    }
}
