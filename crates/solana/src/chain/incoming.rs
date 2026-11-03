use bahaa_chain::{
    ChainError, Movement, ScanCursor, Scanned, TxId, WalkEnd, amount, covered, memo_text, now,
};
use bahaa_config::Feature;
use bahaa_types::{Address, AssetId};
use ruint::aliases::U256;
use serde_json::{Value, json};

use super::{SolanaChain, bytes};
use crate::programs::{MEMO, MEMO_V1, token_account};
use crate::rpc::{self, count, key};
use crate::tx::{self, Key};

const PAGE: usize = 50;
const MAX_LOOKUPS: usize = 20;

struct History {
    address: Key,
    asset: AssetId,
    decimals: u8,
    mint: Option<Key>,
}

struct Listed {
    id: TxId,
    time: u64,
    failed: bool,
}

#[derive(Default)]
struct Found {
    received: Vec<Movement>,
    sent: Vec<Movement>,
}

struct Change {
    index: usize,
    owner: Key,
    delta: i128,
}

impl SolanaChain {
    pub fn can_scan(&self) -> bool {
        self.network
            .endpoints
            .iter()
            .any(|endpoint| endpoint.has(Feature::History))
    }

    pub async fn last_used(&self, address: &Address) -> Result<Option<u64>, ChainError> {
        let newest = self.listed(&bytes(address)?, None, 1).await?;
        Ok(newest.first().map(|entry| entry.time))
    }

    pub async fn scan(
        &self,
        owner: &Address,
        assets: &[AssetId],
        cursor: Option<ScanCursor>,
    ) -> Result<Option<Scanned>, ChainError> {
        let owner = bytes(owner)?;
        let mut histories = Vec::new();
        for asset in assets
            .iter()
            .filter(|asset| asset.network() == &self.network.id)
        {
            histories.push(match asset.contract() {
                None => History {
                    address: owner,
                    asset: asset.clone(),
                    decimals: self.network.native.decimals,
                    mint: None,
                },
                Some(contract) => {
                    let mint = bytes(contract)?;
                    History {
                        address: token_account(&owner, &mint)?,
                        asset: asset.clone(),
                        decimals: self.token_decimals(&mint).await?,
                        mint: Some(mint),
                    }
                }
            });
        }
        let token_accounts: Vec<Key> = histories
            .iter()
            .filter(|history| history.mint.is_some())
            .map(|history| history.address)
            .collect();
        if !token_accounts.is_empty() {
            let opened = self.accounts(&token_accounts, Some(0)).await?;
            histories.retain(|history| {
                history.mint.is_none()
                    || token_accounts
                        .iter()
                        .zip(&opened)
                        .any(|(address, account)| *address == history.address && account.is_some())
            });
        }
        if histories.is_empty() {
            return Ok(None);
        }
        let stop = cursor.map_or(0, |cursor| cursor.next);
        let mut found = Found::default();
        let mut walked = Vec::with_capacity(histories.len());
        for history in &histories {
            walked.push(self.walk(&owner, history, stop, &mut found).await?);
        }
        let next = covered(cursor, &walked, now());
        let Found { received, sent } = found;
        if cursor.is_some_and(|cursor| cursor.next == next.next)
            && received.is_empty()
            && sent.is_empty()
        {
            return Ok(None);
        }
        Ok(Some(Scanned {
            cursor: next,
            found: received,
            sent,
        }))
    }

    async fn walk(
        &self,
        owner: &Key,
        history: &History,
        stop: u64,
        found: &mut Found,
    ) -> Result<(Option<u64>, WalkEnd), ChainError> {
        let mut before: Option<TxId> = None;
        let mut newest = None;
        let mut oldest = None;
        let mut looked = 0;
        let interrupted = |newest, oldest: Option<u64>, err| match oldest {
            Some(time) => Ok((newest, WalkEnd::Capped(time))),
            None => Err(err),
        };
        loop {
            let page = match self.listed(&history.address, before.as_ref(), PAGE).await {
                Ok(page) => page,
                Err(err) => return interrupted(newest, oldest, err),
            };
            for entry in &page {
                newest.get_or_insert(entry.time);
                if entry.time < stop {
                    return Ok((newest, WalkEnd::Connected));
                }
                if looked >= MAX_LOOKUPS {
                    return Ok((newest, WalkEnd::Capped(entry.time)));
                }
                if !entry.failed {
                    looked += 1;
                    let reply = match self.transaction(&entry.id).await {
                        Ok(reply) => reply,
                        Err(err) => return interrupted(newest, oldest, err),
                    };
                    match movement(&reply, owner, history, &entry.id) {
                        Ok(Some((true, moved))) => found.received.push(moved),
                        Ok(Some((false, moved))) => found.sent.push(moved),
                        Ok(None) | Err(_) => {}
                    }
                }
                oldest = Some(entry.time);
            }
            match (page.last(), oldest) {
                (Some(last), _) if page.len() == PAGE => before = Some(last.id.clone()),
                (_, Some(time)) => return Ok((newest, WalkEnd::Start(time))),
                (_, None) => return Ok((newest, WalkEnd::Missing)),
            }
        }
    }

    async fn transaction(&self, id: &TxId) -> Result<Value, ChainError> {
        let reply = self
            .rpc
            .call_with(
                Some(Feature::History),
                "getTransaction",
                json!([id.as_str(), {
                    "encoding": "base64",
                    "commitment": "finalized",
                    "maxSupportedTransactionVersion": 0
                }]),
            )
            .await?;
        if reply.is_null() {
            return Err(ChainError::Network(
                "an endpoint does not know a listed transaction yet".to_owned(),
            ));
        }
        Ok(reply)
    }

    async fn listed(
        &self,
        address: &Key,
        before: Option<&TxId>,
        limit: usize,
    ) -> Result<Vec<Listed>, ChainError> {
        let mut options = json!({"limit": limit, "commitment": "finalized"});
        if let Some(before) = before {
            options["before"] = json!(before.as_str());
        }
        let reply = self
            .rpc
            .call_with(
                Some(Feature::History),
                "getSignaturesForAddress",
                json!([crate::rpc::text(address), options]),
            )
            .await?;
        listed(&reply)
    }
}

fn listed(reply: &Value) -> Result<Vec<Listed>, ChainError> {
    let entries = reply
        .as_array()
        .filter(|entries| entries.len() <= PAGE)
        .ok_or_else(ChainError::malformed)?;
    entries
        .iter()
        .map(|entry| {
            Ok(Listed {
                id: entry["signature"]
                    .as_str()
                    .and_then(|text| TxId::new(text).ok())
                    .ok_or_else(ChainError::malformed)?,
                time: entry["blockTime"].as_u64().unwrap_or(0),
                failed: !entry["err"].is_null(),
            })
        })
        .collect()
}

fn movement(
    found: &Value,
    owner: &Key,
    history: &History,
    id: &TxId,
) -> Result<Option<(bool, Movement)>, ChainError> {
    let raw = rpc::encoded(&found["transaction"])?;
    let wire = tx::fetched(&raw, id)?;
    let meta = &found["meta"];
    if !meta["err"].is_null() {
        return Ok(None);
    }
    let mut keys = wire.keys.clone();
    for side in ["writable", "readonly"] {
        if let Some(loaded) = meta["loadedAddresses"][side].as_array() {
            keys.extend(loaded.iter().map(key).collect::<Result<Vec<Key>, _>>()?);
        }
    }
    let Some(at) = keys.iter().position(|key| *key == history.address) else {
        return Ok(None);
    };
    let tokens = token_changes(meta)?;
    let (delta, other) = if let Some(mint) = history.mint {
        let Some(change) = token_delta(tokens, &mint, at) else {
            return Ok(None);
        };
        change
    } else {
        coin_delta(meta, &keys, &tokens, at)?
    };
    let (Some(other), Ok(units)) = (other, u64::try_from(delta.unsigned_abs())) else {
        return Ok(None);
    };
    if units == 0 {
        return Ok(None);
    }
    let received = delta > 0;
    let memo = wire
        .instructions
        .iter()
        .find(|instruction| {
            keys.get(usize::from(instruction.program))
                .is_some_and(|program| *program == MEMO || *program == MEMO_V1)
        })
        .and_then(|instruction| memo_text(&instruction.data));
    let (from, to) = if received {
        (other, *owner)
    } else {
        (*owner, other)
    };
    Ok(Some((
        received,
        Movement {
            id: id.clone(),
            index: u64::try_from(at).map_err(|_| ChainError::malformed())?,
            from: Address::Solana(from),
            to: Address::Solana(to),
            asset: history.asset.clone(),
            amount: amount(U256::from(units), history.decimals)?,
            block: count(&found["slot"])?,
            time: count(&found["blockTime"])?,
            memo,
        },
    )))
}

fn token_delta(tokens: Vec<(Key, Change)>, mint: &Key, at: usize) -> Option<(i128, Option<Key>)> {
    let same: Vec<Change> = tokens
        .into_iter()
        .filter(|(of, _)| of == mint)
        .map(|(_, change)| change)
        .collect();
    let ours = same.iter().find(|change| change.index == at)?.delta;
    Some((ours, opposite(&same, ours, at).map(|change| change.owner)))
}

fn coin_delta(
    meta: &Value,
    keys: &[Key],
    tokens: &[(Key, Change)],
    at: usize,
) -> Result<(i128, Option<Key>), ChainError> {
    let paid = if at == 0 {
        i128::from(count(&meta["fee"])?)
    } else {
        0
    };
    let plain: Vec<Change> = lamport_changes(meta, keys)?
        .into_iter()
        .filter(|change| !tokens.iter().any(|(_, token)| token.index == change.index))
        .collect();
    let ours = plain
        .iter()
        .find(|change| change.index == at)
        .map_or(0, |change| change.delta)
        + paid;
    Ok((ours, opposite(&plain, ours, at).map(|change| change.owner)))
}

fn lamport_changes(meta: &Value, keys: &[Key]) -> Result<Vec<Change>, ChainError> {
    let list = |field: &str| -> Result<Vec<u64>, ChainError> {
        meta[field]
            .as_array()
            .filter(|values| values.len() == keys.len())
            .ok_or_else(ChainError::malformed)?
            .iter()
            .map(count)
            .collect()
    };
    let (before, after) = (list("preBalances")?, list("postBalances")?);
    Ok(keys
        .iter()
        .zip(before.iter().zip(after))
        .enumerate()
        .map(|(index, (owner, (before, after)))| Change {
            index,
            owner: *owner,
            delta: i128::from(after) - i128::from(*before),
        })
        .collect())
}

fn token_changes(meta: &Value) -> Result<Vec<(Key, Change)>, ChainError> {
    let mut changes: Vec<(Key, Change)> = Vec::new();
    for (field, sign) in [("preTokenBalances", -1), ("postTokenBalances", 1)] {
        let Some(list) = meta[field].as_array() else {
            continue;
        };
        for entry in list {
            let index = usize::try_from(count(&entry["accountIndex"])?)
                .map_err(|_| ChainError::malformed())?;
            let held: u64 = entry["uiTokenAmount"]["amount"]
                .as_str()
                .and_then(|text| text.parse().ok())
                .ok_or_else(ChainError::malformed)?;
            let delta = sign * i128::from(held);
            match changes.iter_mut().find(|(_, change)| change.index == index) {
                Some((_, change)) => change.delta += delta,
                None => changes.push((
                    key(&entry["mint"])?,
                    Change {
                        index,
                        owner: key(&entry["owner"]).unwrap_or_default(),
                        delta,
                    },
                )),
            }
        }
    }
    Ok(changes)
}

fn opposite(changes: &[Change], ours: i128, at: usize) -> Option<&Change> {
    changes
        .iter()
        .filter(|change| change.index != at && change.delta.signum() == -ours.signum())
        .max_by_key(|change| change.delta.unsigned_abs())
}

#[cfg(fuzzing)]
pub(super) fn fuzz(value: &Value) {
    let _ = listed(value);
    let (Ok(id), Ok(asset)) = (TxId::new("1"), "solana".parse::<AssetId>()) else {
        return;
    };
    for mint in [None, Some([0; 32])] {
        let history = History {
            address: [0; 32],
            asset: asset.clone(),
            decimals: 9,
            mint,
        };
        let _ = movement(value, &[0; 32], &history, &id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn recorded(name: &str) -> Value {
        let all: Value =
            serde_json::from_str(include_str!("../../tests/fixtures/history.json")).unwrap();
        all[name].clone()
    }

    fn solana(text: &str) -> Key {
        key(&json!(text)).unwrap()
    }

    fn history(address: &str, mint: Option<&str>) -> History {
        History {
            address: solana(address),
            asset: match mint {
                None => "solana".parse().unwrap(),
                Some(mint) => format!("solana/{mint}").parse().unwrap(),
            },
            decimals: if mint.is_some() { 6 } else { 9 },
            mint: mint.map(solana),
        }
    }

    const BINANCE: &str = "9WzDXwBbmkg8ZTbNMqUxvQRAyrZzDsGYdLVL9zYtAWWM";
    const USDC: &str = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v";
    const BINANCE_USDC: &str = "3hNzYSQs2ziFhnXYzxNqQ1HGnW8pQs9Gf4KfoRErfw9i";

    fn id(found: &Value) -> TxId {
        let raw = rpc::encoded(&found["transaction"]).unwrap();
        tx::id(&tx::read(&raw).unwrap().signatures[0]).unwrap()
    }

    #[test]
    fn a_coin_transfer_reads_from_both_ends() {
        let found = recorded("coins");
        let (received, moved) = movement(
            &found,
            &solana(BINANCE),
            &history(BINANCE, None),
            &id(&found),
        )
        .unwrap()
        .unwrap();
        assert!(received);
        assert_eq!(moved.amount.to_string(), "0.000001");
        assert_eq!(moved.to, Address::Solana(solana(BINANCE)));
        let sender = moved.from;
        let Address::Solana(sender_key) = sender else {
            unreachable!()
        };
        assert_eq!(moved.time, 1_793_637_863);
        assert_eq!(moved.block, 464_566_894);
        let (received, back) = movement(
            &found,
            &sender_key,
            &history(&sender.to_string(), None),
            &id(&found),
        )
        .unwrap()
        .unwrap();
        assert!(!received);
        assert_eq!(back.amount.to_string(), "0.000001");
        assert_eq!(back.to, Address::Solana(solana(BINANCE)));
    }

    #[test]
    fn a_token_transfer_names_the_owners_not_their_token_accounts() {
        let found = recorded("tokens");
        let (received, moved) = movement(
            &found,
            &solana(BINANCE),
            &history(BINANCE_USDC, Some(USDC)),
            &id(&found),
        )
        .unwrap()
        .unwrap();
        assert!(received);
        assert_eq!(moved.amount.to_string(), "1");
        assert!(moved.from.to_string().starts_with("GFJfRAsq"));
        assert_eq!(moved.to, Address::Solana(solana(BINANCE)));
    }

    #[test]
    fn opening_a_token_account_is_not_a_transfer() {
        let found = recorded("opening");
        let raw = rpc::encoded(&found["transaction"]).unwrap();
        let wire = tx::read(&raw).unwrap();
        let payer = wire.keys[0];
        let paying = history(&Address::Solana(payer).to_string(), None);
        assert!(
            movement(&found, &payer, &paying, &id(&found))
                .unwrap()
                .is_none()
        );
        assert!(
            movement(
                &found,
                &solana(BINANCE),
                &history(BINANCE_USDC, Some(USDC)),
                &id(&found)
            )
            .unwrap()
            .is_none()
        );
    }

    #[test]
    fn a_reply_for_another_transaction_is_refused() {
        let found = recorded("coins");
        let other = TxId::new("1111").unwrap();
        assert!(matches!(
            movement(&found, &solana(BINANCE), &history(BINANCE, None), &other),
            Err(ChainError::Network(_))
        ));
        assert!(
            movement(
                &Value::Null,
                &solana(BINANCE),
                &history(BINANCE, None),
                &other
            )
            .is_err()
        );
        let mut failed = found.clone();
        failed["meta"]["err"] = json!({"InstructionError": [0, "InvalidArgument"]});
        assert!(
            movement(
                &failed,
                &solana(BINANCE),
                &history(BINANCE, None),
                &id(&found)
            )
            .unwrap()
            .is_none()
        );
    }

    #[test]
    fn signature_lists_are_read_strictly() {
        let page = json!([
            {"signature": "vGWaZAYtYPZpRsBbKL1LyTZS2Upj7muftZzAH4PJfWif3XAk8vFEiV9XVtDeVRWAURJVpFYBt1sbRXxrdBRMCAQ", "slot": 1, "blockTime": 7, "err": null},
            {"signature": "4UsiwZTYEBqNpUnYAteKAKd2dC1QiaFBNLBpxAWzLLhTAAwTWJb6BHH6FMTBfmbMorNGpruvGnLzzoY7dYU2vyhY", "slot": 2, "blockTime": 6, "err": {"InstructionError": [0, "x"]}}
        ]);
        let read = listed(&page).unwrap();
        assert_eq!(
            (read[0].time, read[0].failed, read[1].failed),
            (7, false, true)
        );
        assert!(listed(&json!([{"signature": "a b"}])).is_err());
        assert!(listed(&json!({})).is_err());
        let long: Vec<Value> = (0..=PAGE).map(|_| page[0].clone()).collect();
        assert!(listed(&json!(long)).is_err());
    }
}
