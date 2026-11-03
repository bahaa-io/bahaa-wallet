use std::collections::BTreeMap;

use alloy_consensus::TxEip1559;
use alloy_primitives::TxKind;
use bahaa_chain::{
    Call, ChainError, ContractSwap, DecodedCall, FeeItem, FeeQuote, PlannedCall, Signed, Step,
    TRANSFER_EVENT, TxId, enough,
};
use bahaa_config::Feature;
use bahaa_crypto::Seed;
use bahaa_types::{Address, Amount, AssetId, to_plain_text};
use futures_util::future::try_join3;
use ruint::aliases::U256;
use serde_json::{Value, json};

use super::{EvmChain, Route, bytes, hex, signed, word};
use crate::rpc::quantity;
use crate::tx;

const APPROVE: [u8; 4] = [0x09, 0x5e, 0xa7, 0xb3];
const ALLOWANCE: &str = "dd62ed3e";
const NATIVE: [u8; 20] = [0xee; 20];
const APPROVAL: &str = "8c5be1e5ebec7d5bd14f71427d1e84f3dd0314c0f7b2291e5b200ac8c7c3b925";
const APPROVAL_FOR_ALL: &str = "17307eab39ab6107e8899845ad3d59bd9653f200f220920489ca2b5937696c31";
const TRANSFER_SINGLE: &str = "c3d58168c5ae7397731d063d5bbf3d657854427343f4c083240f7aacaa2d0f62";
const TRANSFER_BATCH: &str = "4a39dc06d4c0dbc64b70af90fd698a233a518aa5d07e595d983b8c0526c8f7fb";
const MAX_LOGS: usize = 2_048;
const MAX_TOPICS: usize = 4;
const MAX_REASON: usize = 160;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EvmCall {
    planned: PlannedCall,
    tx: TxEip1559,
    fee_max: Amount,
    checked: bool,
}

impl EvmCall {
    pub fn planned(&self) -> &PlannedCall {
        &self.planned
    }

    pub fn fee_max(&self) -> Amount {
        self.fee_max
    }

    pub fn checked(&self) -> bool {
        self.checked
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Gas {
    used: u64,
    limit: u64,
}

#[derive(Debug, PartialEq, Eq)]
struct Simulated {
    ok: bool,
    used: u64,
    peak: u64,
    reason: String,
    output: Vec<u8>,
    logs: Vec<Log>,
}

#[derive(Debug, PartialEq, Eq)]
struct Log {
    address: [u8; 20],
    topics: Vec<[u8; 32]>,
    data: Vec<u8>,
}

#[derive(Default)]
struct Moved {
    came: U256,
    left: U256,
}

impl EvmChain {
    pub async fn read(&self, to: &Address, data: &[u8]) -> Result<Vec<u8>, ChainError> {
        let reply = self
            .rpc
            .call(
                "eth_call",
                json!([{"to": hex(&bytes(to)?), "data": hex(data)}, "latest"]),
            )
            .await?;
        bytes_of(&reply)
    }

    pub async fn prepare_swap(
        &self,
        swap: &ContractSwap,
    ) -> Result<(Vec<EvmCall>, FeeQuote), ChainError> {
        let from = bytes(&swap.from)?;
        self.check_swap(swap).await?;
        let token = swap.sell.contract().map(bytes).transpose()?;
        let spender = swap.spender.as_ref().map(bytes).transpose()?;
        let allowance = async {
            match (token, spender) {
                (Some(token), Some(spender)) => self.allowance(&token, &from, &spender).await,
                _ => Ok(U256::ZERO),
            }
        };
        let (held, nonce, allowance) = try_join3(
            self.held(&from, token.as_ref()),
            self.nonce(&from),
            allowance,
        )
        .await?;
        let units = swap.amount.units();
        let native = (&self.network.native.symbol, self.network.native.decimals);
        match token {
            Some(contract) => enough(
                &self.symbol(&contract),
                units,
                held.token,
                swap.amount.decimals(),
            )?,
            None => enough(native.0, units, held.native, native.1)?,
        }
        let steps = self.steps(swap, allowance)?;
        let (gas, checked) = self.gas(&from, &steps, swap, allowance).await?;
        let (base, tip) = self.fees().await?;
        let max_fee = base
            .checked_mul(U256::from(2u8))
            .and_then(|fee| fee.checked_add(tip))
            .ok_or_else(ChainError::overflow)?;
        let total = |pick: fn(&Gas) -> u64| {
            gas.iter()
                .try_fold(0u64, |sum, gas| sum.checked_add(pick(gas)))
                .ok_or_else(ChainError::overflow)
        };
        let mut fees = self.priced(
            (total(|gas| gas.used)?, total(|gas| gas.limit)?),
            base,
            tip,
            max_fee,
        )?;
        let mut plans = Vec::with_capacity(steps.len());
        for (index, (planned, gas)) in steps.into_iter().zip(gas).enumerate() {
            let tx = TxEip1559 {
                chain_id: self.chain_id,
                nonce: nonce
                    .checked_add(u64::try_from(index).map_err(|_| ChainError::overflow())?)
                    .ok_or_else(ChainError::overflow)?,
                gas_limit: gas.limit,
                max_fee_per_gas: u128::try_from(fees.max_fee)
                    .map_err(|_| ChainError::overflow())?,
                max_priority_fee_per_gas: u128::try_from(fees.tip)
                    .map_err(|_| ChainError::overflow())?,
                to: TxKind::Call(bytes(&planned.call.to)?.into()),
                value: planned.call.value.units(),
                input: planned.call.data.clone().into(),
                ..TxEip1559::default()
            };
            if self.is_rollup() {
                self.add_rollup_fees(&tx, gas.used, &mut fees).await?;
            }
            let fee_max = U256::from(gas.limit)
                .checked_mul(fees.max_fee)
                .ok_or_else(ChainError::overflow)?;
            plans.push(EvmCall {
                planned,
                tx,
                fee_max: self.native(fee_max)?,
                checked,
            });
        }
        fees.lines = merged(fees.lines)?;
        let needed = swap
            .call
            .value
            .units()
            .checked_add(fees.max)
            .ok_or_else(ChainError::overflow)?;
        enough(native.0, needed, held.native, native.1)?;
        Ok((plans, self.fee_quote(fees)?))
    }

    pub async fn recheck(&self, swap: &ContractSwap, plans: &[EvmCall]) -> Result<(), ChainError> {
        let from = bytes(&swap.from)?;
        let allowance = match (swap.sell.contract(), swap.spender) {
            (Some(token), Some(spender)) => {
                self.allowance(&bytes(token)?, &from, &bytes(&spender)?)
                    .await?
            }
            _ => U256::ZERO,
        };
        let steps: Vec<PlannedCall> = plans.iter().map(|plan| plan.planned.clone()).collect();
        let lonely = steps.len() == 1 && swap.spender.is_some();
        if lonely && allowance < swap.amount.units() {
            return Err(ChainError::Invalid(
                "the approval this swap relied on is gone, get a new quote",
            ));
        }
        self.gas(&from, &steps, swap, allowance).await.map(|_| ())
    }

    pub fn sign_call(&self, plan: &EvmCall, seed: &Seed) -> Result<Signed, ChainError> {
        signed(&plan.tx, seed, plan.planned.account, &plan.planned.from)
    }

    pub fn decode_call(&self, signed: &Signed) -> Result<DecodedCall, ChainError> {
        let parsed = tx::parse(&signed.bytes)?;
        if parsed.tx.chain_id != self.chain_id {
            return Err(ChainError::Mismatch("the network"));
        }
        let TxKind::Call(target) = parsed.tx.to else {
            return Err(ChainError::Invalid("contract creation is not a swap"));
        };
        Ok(DecodedCall {
            from: Address::Evm(parsed.from),
            call: Call {
                to: Address::Evm(target.into_array()),
                value: self.native(parsed.tx.value)?,
                data: parsed.tx.input.to_vec(),
            },
            sequence: Some(parsed.tx.nonce),
        })
    }

    pub async fn broadcast_private(&self, signed: &Signed) -> Result<TxId, ChainError> {
        let private = self
            .network
            .endpoints
            .iter()
            .any(|endpoint| endpoint.has(Feature::Private));
        self.send_raw(signed, private.then_some(Feature::Private))
            .await
    }

    async fn check_swap(&self, swap: &ContractSwap) -> Result<(), ChainError> {
        let network = &self.network.id;
        let elsewhere = swap.sell.network() != network
            || swap
                .buy
                .as_ref()
                .is_some_and(|(asset, _)| asset.network() != network);
        if elsewhere {
            return Err(ChainError::Invalid("the asset belongs to another network"));
        }
        if swap
            .buy
            .as_ref()
            .is_some_and(|(asset, _)| *asset == swap.sell)
        {
            return Err(ChainError::Invalid("pick two different assets"));
        }
        if swap.amount.is_zero() {
            return Err(ChainError::Invalid("swap more than zero"));
        }
        if swap.call.data.is_empty() || swap.call.to == swap.from {
            return Err(ChainError::Invalid("a swap must call a contract"));
        }
        bytes(&swap.call.to)?;
        if swap.call.value.decimals() != self.network.native.decimals {
            return Err(ChainError::Invalid(
                "the amount uses the wrong number of decimals",
            ));
        }
        let decimals = match swap.sell.contract() {
            None if swap.call.value == swap.amount && swap.spender.is_none() => {
                self.network.native.decimals
            }
            Some(contract)
                if swap.call.value.is_zero()
                    && swap.spender.is_some()
                    && *contract != swap.call.to =>
            {
                self.token_decimals(&bytes(contract)?).await?
            }
            _ => {
                return Err(ChainError::Invalid(
                    "the swap does not pay for itself the way it claims",
                ));
            }
        };
        if swap.amount.decimals() != decimals {
            return Err(ChainError::Invalid(
                "the amount uses the wrong number of decimals",
            ));
        }
        Ok(())
    }

    async fn allowance(
        &self,
        token: &[u8; 20],
        owner: &[u8; 20],
        spender: &[u8; 20],
    ) -> Result<U256, ChainError> {
        self.call_uint(
            token,
            &format!("0x{ALLOWANCE}{}{}", word(owner), word(spender)),
        )
        .await
    }

    fn steps(&self, swap: &ContractSwap, allowance: U256) -> Result<Vec<PlannedCall>, ChainError> {
        let mut steps = Vec::new();
        if let (Some(contract), Some(spender)) = (swap.sell.contract(), swap.spender)
            && allowance < swap.amount.units()
        {
            if !allowance.is_zero() {
                let nothing =
                    Amount::zero(swap.amount.decimals()).map_err(|_| ChainError::overflow())?;
                steps.push(self.approval(swap, contract, spender, nothing)?);
            }
            steps.push(self.approval(swap, contract, spender, swap.amount)?);
        }
        steps.push(PlannedCall {
            step: Step::Swap,
            account: swap.account,
            from: swap.from,
            call: swap.call.clone(),
        });
        Ok(steps)
    }

    fn approval(
        &self,
        swap: &ContractSwap,
        contract: &Address,
        spender: Address,
        amount: Amount,
    ) -> Result<PlannedCall, ChainError> {
        Ok(PlannedCall {
            step: Step::Approve {
                asset: swap.sell.clone(),
                spender,
                amount,
            },
            account: swap.account,
            from: swap.from,
            call: Call {
                to: *contract,
                value: self.native(U256::ZERO)?,
                data: approve_data(&bytes(&spender)?, amount.units()),
            },
        })
    }

    async fn gas(
        &self,
        from: &[u8; 20],
        steps: &[PlannedCall],
        swap: &ContractSwap,
        allowance: U256,
    ) -> Result<(Vec<Gas>, bool), ChainError> {
        let mut calls = steps
            .iter()
            .map(|step| call_json(from, &step.call))
            .collect::<Result<Vec<_>, ChainError>>()?;
        let check = match (swap.sell.contract(), swap.spender) {
            (Some(token), Some(spender)) => {
                calls.push(json!({
                    "from": hex(from),
                    "to": hex(&bytes(token)?),
                    "data": format!("0x{ALLOWANCE}{}{}", word(from), word(&bytes(&spender)?)),
                }));
                Some(if steps.len() > 1 {
                    U256::ZERO
                } else {
                    allowance
                        .checked_sub(swap.amount.units())
                        .ok_or_else(ChainError::overflow)?
                })
            }
            _ => None,
        };
        let params = json!([{
            "blockStateCalls": [{"calls": calls}],
            "traceTransfers": true,
            "validation": false,
        }, "latest"]);
        match self.rpc.call_if_served("eth_simulateV1", params).await? {
            Some(reply) => {
                let simulated = simulated(&reply, calls.len())?;
                judge(from, swap, steps.len() - 1, &simulated, check)?;
                let gas = simulated[..steps.len()]
                    .iter()
                    .map(|call| Gas {
                        used: call.used,
                        limit: call.peak.saturating_add(call.peak / 4),
                    })
                    .collect();
                Ok((gas, true))
            }
            None if swap.ours && steps.len() == 1 => {
                let route = Route {
                    call_to: bytes(&swap.call.to)?,
                    value: swap.call.value.units(),
                    data: swap.call.data.clone(),
                    token: None,
                    decimals: self.network.native.decimals,
                };
                let estimate = self.estimate(from, &route).await?;
                let gas = vec![Gas {
                    used: estimate,
                    limit: estimate.saturating_add(estimate / 5),
                }];
                Ok((gas, false))
            }
            None => Err(ChainError::Unsupported(
                "checking this swap before it is signed",
            )),
        }
    }
}

fn call_json(from: &[u8; 20], call: &Call) -> Result<Value, ChainError> {
    Ok(json!({
        "from": hex(from),
        "to": hex(&bytes(&call.to)?),
        "value": format!("{:#x}", call.value.units()),
        "data": hex(&call.data),
    }))
}

fn approve_data(spender: &[u8; 20], units: U256) -> Vec<u8> {
    let mut data = Vec::with_capacity(68);
    data.extend_from_slice(&APPROVE);
    data.extend_from_slice(&[0u8; 12]);
    data.extend_from_slice(spender);
    data.extend_from_slice(&units.to_be_bytes::<32>());
    data
}

fn merged(lines: Vec<(FeeItem, U256)>) -> Result<Vec<(FeeItem, U256)>, ChainError> {
    let mut merged: Vec<(FeeItem, U256)> = Vec::with_capacity(lines.len());
    for (item, units) in lines {
        match merged.iter_mut().find(|(seen, _)| *seen == item) {
            Some((_, total)) => {
                *total = total.checked_add(units).ok_or_else(ChainError::overflow)?;
            }
            None => merged.push((item, units)),
        }
    }
    Ok(merged)
}

fn bytes_of(value: &Value) -> Result<Vec<u8>, ChainError> {
    value
        .as_str()
        .and_then(|text| text.strip_prefix("0x"))
        .and_then(bahaa_types::hex::decode)
        .ok_or_else(ChainError::malformed)
}

fn fixed<const N: usize>(value: &Value) -> Result<[u8; N], ChainError> {
    bytes_of(value)?
        .try_into()
        .map_err(|_| ChainError::malformed())
}

fn gas_of(value: &Value) -> Result<u64, ChainError> {
    u64::try_from(quantity(value)?).map_err(|_| ChainError::malformed())
}

fn simulated(reply: &Value, count: usize) -> Result<Vec<Simulated>, ChainError> {
    let calls = reply
        .as_array()
        .filter(|blocks| blocks.len() == 1)
        .and_then(|blocks| blocks[0]["calls"].as_array())
        .filter(|calls| calls.len() == count)
        .ok_or_else(ChainError::malformed)?;
    calls
        .iter()
        .map(|call| {
            let used = gas_of(&call["gasUsed"])?;
            let peak = match call.get("maxUsedGas") {
                Some(peak) => gas_of(peak)?.max(used),
                None => used,
            };
            let logs = call["logs"]
                .as_array()
                .filter(|logs| logs.len() <= MAX_LOGS)
                .ok_or_else(ChainError::malformed)?
                .iter()
                .map(|log| {
                    let topics = log["topics"]
                        .as_array()
                        .filter(|topics| topics.len() <= MAX_TOPICS)
                        .ok_or_else(ChainError::malformed)?
                        .iter()
                        .map(fixed::<32>)
                        .collect::<Result<Vec<_>, _>>()?;
                    Ok(Log {
                        address: fixed::<20>(&log["address"])?,
                        topics,
                        data: bytes_of(&log["data"])?,
                    })
                })
                .collect::<Result<Vec<_>, ChainError>>()?;
            Ok(Simulated {
                ok: call["status"] == "0x1",
                used,
                peak,
                reason: to_plain_text(
                    call["error"]["message"].as_str().unwrap_or_default(),
                    MAX_REASON,
                ),
                output: bytes_of(&call["returnData"])?,
                logs,
            })
        })
        .collect()
}

fn key_of(asset: &AssetId) -> Result<[u8; 20], ChainError> {
    match asset.contract() {
        None => Ok(NATIVE),
        Some(contract) => bytes(contract),
    }
}

fn address_in(topic: &[u8; 32]) -> Option<[u8; 20]> {
    topic[..12]
        .iter()
        .all(|byte| *byte == 0)
        .then(|| topic[12..].try_into().ok())
        .flatten()
}

fn tally(
    owner: &[u8; 20],
    spender: Option<[u8; 20]>,
    approvals: usize,
    simulated: &[Simulated],
) -> Result<BTreeMap<[u8; 20], Moved>, ChainError> {
    let mut moved: BTreeMap<[u8; 20], Moved> = BTreeMap::new();
    let ours = |topic: Option<&[u8; 32]>| topic.and_then(address_in).as_ref() == Some(owner);
    for (index, call) in simulated.iter().enumerate() {
        for log in &call.logs {
            let Some(event) = log
                .topics
                .first()
                .map(|topic| bahaa_types::hex::encode(topic))
            else {
                continue;
            };
            match (event.as_str(), log.topics.len()) {
                (TRANSFER_EVENT, 3) => {
                    let value = <[u8; 32]>::try_from(log.data.as_slice())
                        .map(|word| U256::from_be_bytes(word))
                        .map_err(|_| {
                            ChainError::Invalid(
                                "the swap moves tokens in a way this wallet can't read",
                            )
                        })?;
                    let entry = moved.entry(log.address).or_default();
                    if ours(log.topics.get(1)) {
                        entry.left = entry
                            .left
                            .checked_add(value)
                            .ok_or_else(ChainError::overflow)?;
                    }
                    if ours(log.topics.get(2)) {
                        entry.came = entry
                            .came
                            .checked_add(value)
                            .ok_or_else(ChainError::overflow)?;
                    }
                }
                (TRANSFER_EVENT, 4) if ours(log.topics.get(1)) => {
                    return Err(ChainError::Invalid(
                        "the swap would move a collectible out of this wallet",
                    ));
                }
                (TRANSFER_SINGLE | TRANSFER_BATCH, 4) if ours(log.topics.get(2)) => {
                    return Err(ChainError::Invalid(
                        "the swap would move a collectible out of this wallet",
                    ));
                }
                (APPROVAL, 3)
                    if index >= approvals
                        && ours(log.topics.get(1))
                        && log.topics.get(2).and_then(address_in) != spender =>
                {
                    return Err(ChainError::Invalid(
                        "the swap would give someone access to this wallet's assets",
                    ));
                }
                (APPROVAL_FOR_ALL, _) if ours(log.topics.get(1)) => {
                    return Err(ChainError::Invalid(
                        "the swap would give someone access to this wallet's assets",
                    ));
                }
                _ => {}
            }
        }
    }
    Ok(moved)
}

fn judge(
    owner: &[u8; 20],
    swap: &ContractSwap,
    approvals: usize,
    simulated: &[Simulated],
    remaining: Option<U256>,
) -> Result<(), ChainError> {
    if let Some(failed) = simulated.iter().find(|call| !call.ok) {
        let reason = if failed.reason.is_empty() {
            "the contract gave no reason"
        } else {
            failed.reason.as_str()
        };
        return Err(ChainError::Rejected(format!(
            "the swap would fail: {reason}"
        )));
    }
    let spender = swap.spender.as_ref().map(bytes).transpose()?;
    let moved = tally(owner, spender, approvals, simulated)?;
    let sold = key_of(&swap.sell)?;
    let bought = swap
        .buy
        .as_ref()
        .map(|(asset, _)| key_of(asset))
        .transpose()?;
    for (asset, moved) in &moved {
        if *asset == sold {
            let net = moved.left.checked_sub(moved.came);
            if net != Some(swap.amount.units()) {
                return Err(ChainError::Invalid(
                    "the swap would take another amount than the one shown",
                ));
            }
        } else if moved.left > moved.came {
            return Err(ChainError::Invalid(
                "the swap would take other assets from this wallet",
            ));
        }
    }
    if !moved.contains_key(&sold) {
        return Err(ChainError::Invalid(
            "the swap would take another amount than the one shown",
        ));
    }
    if let (Some(bought), Some((_, minimum))) = (bought, &swap.buy) {
        let received = moved
            .get(&bought)
            .and_then(|moved| moved.came.checked_sub(moved.left))
            .unwrap_or(U256::ZERO);
        if received < minimum.units() {
            return Err(ChainError::Invalid(
                "the swap would pay less than its minimum",
            ));
        }
    }
    if let Some(remaining) = remaining {
        let left = simulated
            .last()
            .and_then(|call| <[u8; 32]>::try_from(call.output.as_slice()).ok())
            .map(U256::from_be_bytes);
        if left != Some(remaining) {
            return Err(ChainError::Invalid(
                "the swap would leave an open approval behind",
            ));
        }
    }
    Ok(())
}

#[cfg(fuzzing)]
pub(crate) fn fuzz(value: &Value) {
    let count = value[0]["calls"].as_array().map_or(0, Vec::len);
    if let (Ok(simulated), Ok(sell)) = (simulated(value, count), "bsc".parse::<AssetId>()) {
        let swap = ContractSwap {
            account: 0,
            from: Address::Evm([1; 20]),
            call: Call {
                to: Address::Evm([2; 20]),
                value: Amount::default(),
                data: vec![1],
            },
            spender: None,
            sell,
            amount: Amount::default(),
            buy: None,
            ours: false,
        };
        let _ = judge(&[1; 20], &swap, 0, &simulated, Some(U256::ZERO));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ME: [u8; 20] = [0x11; 20];
    const ROUTER: [u8; 20] = [0x22; 20];
    const USDT: [u8; 20] = [0x55; 20];
    const STRANGER: [u8; 20] = [0x99; 20];

    fn topic(address: &[u8; 20]) -> String {
        format!("0x{:0>64}", bahaa_types::hex::encode(address))
    }

    fn moved(token: &[u8; 20], from: &[u8; 20], to: &[u8; 20], units: u64) -> Value {
        json!({
            "address": hex(token),
            "topics": [format!("0x{TRANSFER_EVENT}"), topic(from), topic(to)],
            "data": format!("0x{units:064x}"),
        })
    }

    fn reply(calls: Vec<Vec<Value>>, remaining: Option<u64>) -> Value {
        let mut list: Vec<Value> = calls
            .into_iter()
            .map(|logs| json!({"status": "0x1", "gasUsed": "0x5208", "returnData": "0x", "logs": logs}))
            .collect();
        if let Some(left) = remaining {
            list.push(json!({
                "status": "0x1",
                "gasUsed": "0x5f76",
                "returnData": format!("0x{left:064x}"),
                "logs": [],
            }));
        }
        json!([{"calls": list}])
    }

    fn selling_usdt() -> ContractSwap {
        ContractSwap {
            account: 0,
            from: Address::Evm(ME),
            call: Call {
                to: Address::Evm(ROUTER),
                value: Amount::parse("0", 18).unwrap(),
                data: vec![1],
            },
            spender: Some(Address::Evm(ROUTER)),
            sell: format!("bsc/{}", Address::Evm(USDT)).parse().unwrap(),
            amount: Amount::parse("0.000000000000000100", 18).unwrap(),
            buy: Some((
                "bsc".parse().unwrap(),
                Amount::parse("0.000000000000000040", 18).unwrap(),
            )),
            ours: true,
        }
    }

    fn judged(reply: &Value, approvals: usize, remaining: Option<U256>) -> Result<(), ChainError> {
        let count = reply[0]["calls"].as_array().unwrap().len();
        judge(
            &ME,
            &selling_usdt(),
            approvals,
            &simulated(reply, count)?,
            remaining,
        )
    }

    fn honest() -> Vec<Value> {
        vec![
            moved(&USDT, &ME, &ROUTER, 100),
            moved(&NATIVE, &ROUTER, &ME, 41),
        ]
    }

    #[test]
    fn an_exact_swap_with_its_approval_used_up_passes() {
        let ok = reply(vec![vec![], honest()], Some(0));
        assert_eq!(judged(&ok, 1, Some(U256::ZERO)), Ok(()));
        let gas = simulated(&ok, 3).unwrap();
        assert_eq!((gas[0].used, gas[0].peak), (21_000, 21_000));
    }

    #[test]
    fn taking_more_or_other_assets_or_paying_less_is_refused() {
        let mut more = honest();
        more[0] = moved(&USDT, &ME, &ROUTER, 101);
        assert!(judged(&reply(vec![vec![], more], Some(0)), 1, Some(U256::ZERO)).is_err());
        let mut other = honest();
        other.push(moved(&[0x66; 20], &ME, &STRANGER, 1));
        assert_eq!(
            judged(&reply(vec![vec![], other], Some(0)), 1, Some(U256::ZERO)),
            Err(ChainError::Invalid(
                "the swap would take other assets from this wallet"
            ))
        );
        let mut less = honest();
        less[1] = moved(&NATIVE, &ROUTER, &ME, 39);
        assert_eq!(
            judged(&reply(vec![vec![], less], Some(0)), 1, Some(U256::ZERO)),
            Err(ChainError::Invalid(
                "the swap would pay less than its minimum"
            ))
        );
        let mut forwarded = honest();
        forwarded.push(moved(&NATIVE, &ME, &STRANGER, 41));
        assert!(
            judged(
                &reply(vec![vec![], forwarded], Some(0)),
                1,
                Some(U256::ZERO)
            )
            .is_err()
        );
    }

    #[test]
    fn an_approval_left_open_or_given_to_someone_else_is_refused() {
        let ok = reply(vec![vec![], honest()], Some(7));
        assert_eq!(
            judged(&ok, 1, Some(U256::ZERO)),
            Err(ChainError::Invalid(
                "the swap would leave an open approval behind"
            ))
        );
        let mut sneaky = honest();
        sneaky.push(json!({
            "address": hex(&USDT),
            "topics": [format!("0x{APPROVAL}"), topic(&ME), topic(&STRANGER)],
            "data": format!("0x{:064x}", 1),
        }));
        assert_eq!(
            judged(&reply(vec![vec![], sneaky], Some(0)), 1, Some(U256::ZERO)),
            Err(ChainError::Invalid(
                "the swap would give someone access to this wallet's assets"
            ))
        );
        let mut lowered = honest();
        lowered.push(json!({
            "address": hex(&USDT),
            "topics": [format!("0x{APPROVAL}"), topic(&ME), topic(&ROUTER)],
            "data": format!("0x{:064x}", 0),
        }));
        assert_eq!(
            judged(&reply(vec![vec![], lowered], Some(0)), 1, Some(U256::ZERO)),
            Ok(())
        );
    }

    #[test]
    fn a_failing_call_reports_the_contracts_reason() {
        let mut failed = reply(vec![vec![], honest()], Some(0));
        failed[0]["calls"][1]["status"] = json!("0x0");
        failed[0]["calls"][1]["error"] =
            json!({"message": "execution reverted: Too little received\u{202e}"});
        assert_eq!(
            judged(&failed, 1, Some(U256::ZERO)),
            Err(ChainError::Rejected(
                "the swap would fail: execution reverted: Too little received".to_owned()
            ))
        );
    }

    #[test]
    fn approvals_are_exact_and_merged_fee_lines_add_up() {
        let data = approve_data(&ROUTER, U256::from(5u8));
        assert_eq!(data[..4], APPROVE);
        assert_eq!(data[16..36], ROUTER);
        assert_eq!(data[67], 5);
        let lines = merged(vec![
            (FeeItem::L1Data, U256::from(2u8)),
            (FeeItem::Operator, U256::from(1u8)),
            (FeeItem::L1Data, U256::from(3u8)),
        ])
        .unwrap();
        assert_eq!(
            lines,
            vec![
                (FeeItem::L1Data, U256::from(5u8)),
                (FeeItem::Operator, U256::from(1u8))
            ]
        );
    }
}
