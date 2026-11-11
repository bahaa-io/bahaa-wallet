use bahaa_chain::{ChainError, Incoming, Movement, Outgoing, TxId, Watch};
use bahaa_types::{Address, AssetId};
use futures_util::future::try_join;
use ruint::aliases::U256;
use serde_json::{Value, json};

use super::logs::{REACH, block_number, digits, malformed, number};
use super::{EvmChain, bytes, hex, memo, word};
use crate::rpc::quantity;

const MAX_STATES: usize = 48;
const MAX_BLOCKS: usize = 12;
const NATIVE_INDEX: u64 = u64::MAX;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct State {
    block: u64,
    balance: U256,
    nonce: u64,
}

impl State {
    fn same(&self, other: &Self) -> bool {
        self.balance == other.balance && self.nonce == other.nonce
    }
}

pub(super) struct Native {
    pub watch: Option<Watch>,
    pub found: Vec<Incoming>,
    pub sent: Vec<Outgoing>,
}

impl Native {
    pub(super) fn unchanged(watch: Option<Watch>) -> Self {
        Self {
            watch,
            found: Vec::new(),
            sent: Vec::new(),
        }
    }
}

struct Seen {
    id: String,
    from: [u8; 20],
    to: Option<[u8; 20]>,
    value: U256,
    input: Vec<u8>,
}

impl EvmChain {
    pub(super) async fn watched(
        &self,
        owner: &Address,
        watch: Option<Watch>,
        until: u64,
    ) -> Result<Native, ChainError> {
        if watch.is_some_and(|watch| watch.block >= until) {
            return Ok(Native::unchanged(watch));
        }
        let ours = bytes(owner)?;
        let mut reads = 0;
        let top = self.state(&ours, until, &mut reads).await?;
        let base = if let Some(watch) = watch {
            State {
                block: watch.block,
                balance: watch.balance.units(),
                nonce: watch.nonce,
            }
        } else {
            let start = until.saturating_sub(REACH - 1);
            match self.state(&ours, start, &mut reads).await {
                Ok(state) => state,
                Err(ChainError::Pruned) => self
                    .edge(&ours, start, top, &mut reads)
                    .await?
                    .unwrap_or(top),
                Err(err) => return Err(err),
            }
        };
        let (changes, mut reached) = self.changes(&ours, base, top, &mut reads).await?;
        let mut native = Native::unchanged(None);
        for (index, (before, after)) in changes.iter().enumerate() {
            if index == MAX_BLOCKS {
                reached = *before;
                break;
            }
            self.block_moves(owner, before, after, &mut native).await?;
        }
        native.watch = Some(Watch {
            block: reached.block,
            balance: self.native(reached.balance)?,
            nonce: reached.nonce,
        });
        Ok(native)
    }

    async fn state(
        &self,
        ours: &[u8; 20],
        block: u64,
        reads: &mut usize,
    ) -> Result<State, ChainError> {
        *reads = reads.saturating_add(1);
        let at = number(block);
        let (balance, nonce) = try_join(
            self.rpc.call("eth_getBalance", json!([hex(ours), at])),
            self.rpc
                .call("eth_getTransactionCount", json!([hex(ours), at])),
        )
        .await?;
        Ok(State {
            block,
            balance: quantity(&balance)?,
            nonce: u64::try_from(quantity(&nonce)?).map_err(|_| malformed())?,
        })
    }

    async fn changes(
        &self,
        ours: &[u8; 20],
        base: State,
        top: State,
        reads: &mut usize,
    ) -> Result<(Vec<(State, State)>, State), ChainError> {
        let mut spans = vec![(base, top)];
        let mut changes = Vec::new();
        while let Some((low, high)) = spans.pop() {
            if low.same(&high) {
                continue;
            }
            if high.block <= low.block.saturating_add(1) {
                changes.push((low, high));
                continue;
            }
            if *reads >= MAX_STATES {
                return Ok((changes, low));
            }
            let middle = low.block + (high.block - low.block) / 2;
            match self.state(ours, middle, reads).await {
                Ok(state) => {
                    spans.push((state, high));
                    spans.push((low, state));
                }
                Err(ChainError::Pruned) => match self.edge(ours, middle, high, reads).await? {
                    Some(edge) => spans.push((edge, high)),
                    None => return Ok((changes, low)),
                },
                Err(err) => return Err(err),
            }
        }
        Ok((changes, top))
    }

    async fn edge(
        &self,
        ours: &[u8; 20],
        mut pruned: u64,
        mut readable: State,
        reads: &mut usize,
    ) -> Result<Option<State>, ChainError> {
        while readable.block > pruned.saturating_add(1) {
            if *reads >= MAX_STATES {
                return Ok(None);
            }
            let middle = pruned + (readable.block - pruned) / 2;
            match self.state(ours, middle, reads).await {
                Ok(state) => readable = state,
                Err(ChainError::Pruned) => pruned = middle,
                Err(err) => return Err(err),
            }
        }
        Ok(Some(readable))
    }

    async fn block_moves(
        &self,
        owner: &Address,
        before: &State,
        after: &State,
        native: &mut Native,
    ) -> Result<(), ChainError> {
        let ours = bytes(owner)?;
        let block = self
            .rpc
            .call("eth_getBlockByNumber", json!([number(after.block), true]))
            .await?;
        if block_number(&block["number"])? != after.block {
            return Err(malformed());
        }
        let time = block_number(&block["timestamp"])?;
        let moved = |id: &str, from: [u8; 20], to: [u8; 20], units: U256, memo: Option<String>| {
            Ok::<_, ChainError>(Movement {
                id: TxId::new(id)?,
                index: NATIVE_INDEX,
                from: Address::Evm(from),
                to: Address::Evm(to),
                asset: AssetId::Native(self.network.id.clone()),
                amount: self.native(units)?,
                block: after.block,
                time,
                memo,
            })
        };
        let mut paid = false;
        let listed = block["transactions"].as_array().ok_or_else(malformed)?;
        for seen in listed.iter().filter_map(read_transaction) {
            if seen.value.is_zero() || seen.to == Some(seen.from) {
                continue;
            }
            if seen.to == Some(ours) {
                paid = true;
                native.found.push(moved(
                    &seen.id,
                    seen.from,
                    ours,
                    seen.value,
                    memo(&seen.input),
                )?);
            } else if seen.from == ours
                && let Some(to) = seen.to
            {
                native
                    .sent
                    .push(moved(&seen.id, ours, to, seen.value, memo(&seen.input))?);
            }
        }
        let gained = after
            .balance
            .checked_sub(before.balance)
            .filter(|gained| !gained.is_zero());
        if !paid
            && after.nonce == before.nonce
            && let Some(gained) = gained
            && let Some((id, payer)) = self.payer(&ours, after.block).await
        {
            native.found.push(moved(&id, payer, ours, gained, None)?);
        }
        Ok(())
    }

    async fn payer(&self, ours: &[u8; 20], block: u64) -> Option<(String, [u8; 20])> {
        let receipts = self
            .rpc
            .call("eth_getBlockReceipts", json!([number(block)]))
            .await
            .ok()?;
        let ours = word(ours);
        let mut payers: Vec<(String, [u8; 20])> = Vec::new();
        for receipt in receipts.as_array()? {
            let named = receipt["logs"]
                .as_array()
                .is_some_and(|logs| logs.iter().any(|log| names(log, &ours)));
            let Some(found) = named.then(|| payer_of(receipt)).flatten() else {
                continue;
            };
            if !payers.iter().any(|(id, _)| *id == found.0) {
                payers.push(found);
            }
        }
        match payers.as_slice() {
            [only] => Some(only.clone()),
            _ => None,
        }
    }
}

fn read_transaction(tx: &Value) -> Option<Seen> {
    let id = tx["hash"].as_str()?.to_ascii_lowercase();
    digits(&id, 64)?;
    Some(Seen {
        id,
        from: address(tx["from"].as_str()?)?,
        to: match &tx["to"] {
            Value::Null => None,
            to => Some(address(to.as_str()?)?),
        },
        value: quantity(&tx["value"]).ok()?,
        input: bahaa_types::hex::decode(tx["input"].as_str()?.strip_prefix("0x")?)?,
    })
}

fn payer_of(receipt: &Value) -> Option<(String, [u8; 20])> {
    let id = receipt["transactionHash"].as_str()?.to_ascii_lowercase();
    digits(&id, 64)?;
    Some((id, address(receipt["to"].as_str()?)?))
}

fn names(log: &Value, ours: &str) -> bool {
    let matches = |text: &str| text.eq_ignore_ascii_case(ours);
    let topics = log["topics"].as_array().is_some_and(|topics| {
        topics
            .iter()
            .filter_map(|topic| topic.as_str()?.strip_prefix("0x"))
            .any(matches)
    });
    let data = log["data"]
        .as_str()
        .and_then(|data| data.strip_prefix("0x"))
        .is_some_and(|data| {
            data.len() % 64 == 0
                && data
                    .as_bytes()
                    .chunks(64)
                    .any(|chunk| chunk.eq_ignore_ascii_case(ours.as_bytes()))
        });
    topics || data
}

fn address(text: &str) -> Option<[u8; 20]> {
    bahaa_types::hex::decode_array(digits(text, 40)?)
}

#[cfg(fuzzing)]
pub(super) fn fuzz(value: &Value) {
    for item in value
        .as_array()
        .map_or(std::slice::from_ref(value), Vec::as_slice)
    {
        let _ = read_transaction(item);
        let _ = payer_of(item);
        let _ = names(item, &word(&[0; 20]));
    }
}
