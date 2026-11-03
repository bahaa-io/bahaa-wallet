use bahaa_chain::{
    Call, ChainError, ContractSwap, DecodedCall, FeeItem, FeeQuote, PlannedCall, Signed, Step,
    TxId, enough,
};
use bahaa_crypto::Seed;
use bahaa_types::{Address, Amount, hex};
use ruint::aliases::U256;
use serde_json::json;

use super::{BALANCE_OF, TronChain, amount, bytes, key, tron_hex, word};
use crate::fees::{self, Quote, Resources};
use crate::proto::Raw;
use crate::tx;

const ALLOWANCE: &str = "allowance(address,address)";
const APPROVE: [u8; 4] = [0x09, 0x5e, 0xa7, 0xb3];
const SWAP_ENERGY: u64 = 200_000;
const WORD: usize = 32;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TronCall {
    planned: PlannedCall,
    raw: Raw,
    fee_max: Amount,
    checked: bool,
}

impl TronCall {
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

impl TronChain {
    pub async fn read(&self, contract: &Address, data: &[u8]) -> Result<Vec<u8>, ChainError> {
        let contract = bytes(contract)?;
        let reply = self
            .trigger(json!({
                "owner_address": tron_hex(&contract),
                "contract_address": tron_hex(&contract),
                "data": hex::encode(data),
            }))
            .await?;
        reply["constant_result"]
            .as_array()
            .filter(|results| results.len() == 1)
            .and_then(|results| results[0].as_str())
            .and_then(hex::decode)
            .ok_or_else(ChainError::malformed)
    }

    pub async fn prepare_swap(
        &self,
        swap: &ContractSwap,
    ) -> Result<(Vec<TronCall>, FeeQuote), ChainError> {
        self.check_swap(swap)?;
        let from = bytes(&swap.from)?;
        let router = bytes(&swap.call.to)?;
        let balance = self
            .account(&from)
            .await?
            .map_or(U256::ZERO, |(free, _)| free);
        let mut steps = Vec::new();
        if let (Some(token), Some(spender)) = (swap.sell.contract(), swap.spender) {
            let token = bytes(token)?;
            let held = self.call(&from, &token, BALANCE_OF, &word(&from)).await?;
            enough(
                &self.symbol(&token),
                swap.amount.units(),
                held,
                swap.amount.decimals(),
            )?;
            let allowance = self
                .call(
                    &from,
                    &token,
                    ALLOWANCE,
                    &format!("{}{}", word(&from), word(&bytes(&spender)?)),
                )
                .await?;
            if allowance < swap.amount.units() {
                if !allowance.is_zero() {
                    steps.push(self.approval(swap, &token, &spender, U256::ZERO)?);
                }
                steps.push(self.approval(swap, &token, &spender, swap.amount.units())?);
            }
        }
        let ready = steps.is_empty();
        steps.push((
            Step::Swap,
            swap.call.clone(),
            router,
            i64::try_from(swap.call.value.units()).map_err(|_| ChainError::overflow())?,
        ));
        let prices = self.prices().await?;
        let mut resources = self.resources(&from).await?;
        let anchor = self.anchor().await?;
        let mut total = Quote::default();
        let count = steps.len();
        let mut plans = Vec::with_capacity(count);
        for (index, (step, call, contract, value)) in steps.into_iter().enumerate() {
            let last = index + 1 == count;
            let energy = if last && !ready {
                SWAP_ENERGY
            } else {
                self.checked_call(&from, &contract, &call.data, value, swap, last)
                    .await?
            };
            let limit = fees::fee_limit(energy, &prices)?;
            let raw = tx::raw(
                tx::call(&from, &contract, call.data.clone(), value),
                &anchor,
                limit,
                None,
            )?;
            let quote = fees::quote(
                tx::bandwidth(&raw),
                Some((energy, limit)),
                (false, false),
                &resources,
                &prices,
            )?;
            resources = used(resources, tx::bandwidth(&raw), energy);
            let fee_max = amount(U256::from(quote.max), self.network.native.decimals)?;
            merge(&mut total, quote)?;
            plans.push(TronCall {
                planned: PlannedCall {
                    step,
                    account: swap.account,
                    from: swap.from,
                    call,
                },
                raw,
                fee_max,
                checked: ready,
            });
        }
        let needed = swap
            .call
            .value
            .units()
            .checked_add(U256::from(total.max))
            .ok_or_else(ChainError::overflow)?;
        let native = &self.network.native;
        enough(&native.symbol, needed, balance, native.decimals)?;
        Ok((plans, self.fee_quote(total)?))
    }

    pub async fn recheck(&self, swap: &ContractSwap) -> Result<(), ChainError> {
        self.check_swap(swap)?;
        if swap.spender.is_some() {
            return Ok(());
        }
        let from = bytes(&swap.from)?;
        let value = i64::try_from(swap.call.value.units()).map_err(|_| ChainError::overflow())?;
        self.checked_call(
            &from,
            &bytes(&swap.call.to)?,
            &swap.call.data,
            value,
            swap,
            true,
        )
        .await
        .map(|_| ())
    }

    pub fn sign_call(&self, plan: &TronCall, seed: &Seed) -> Result<Signed, ChainError> {
        let key = key(seed, plan.planned.account)?;
        if Address::Tron(bahaa_crypto::secp256k1_address(&key.public_key())) != plan.planned.from {
            return Err(ChainError::Invalid(
                "this seed does not own the sending account",
            ));
        }
        let (bytes, id) = tx::sign(plan.raw.clone(), &key)?;
        Ok(Signed {
            id: TxId::new(hex::encode(&id))?,
            bytes,
        })
    }

    pub fn decode_call(&self, signed: &Signed) -> Result<DecodedCall, ChainError> {
        let parsed = tx::parse_call(&signed.bytes)?;
        if hex::encode(&parsed.id) != signed.id.as_str() {
            return Err(ChainError::Mismatch("the transaction id"));
        }
        Ok(DecodedCall {
            from: Address::Tron(parsed.from),
            call: Call {
                to: Address::Tron(parsed.contract),
                value: amount(U256::from(parsed.call_value), self.network.native.decimals)?,
                data: parsed.data,
            },
            sequence: None,
        })
    }

    fn check_swap(&self, swap: &ContractSwap) -> Result<(), ChainError> {
        let network = &self.network.id;
        let elsewhere = swap.sell.network() != network
            || swap
                .buy
                .as_ref()
                .is_some_and(|(asset, _)| asset.network() != network);
        if elsewhere || swap.amount.is_zero() || !swap.ours || swap.call.data.is_empty() {
            return Err(ChainError::Invalid("not a swap this wallet builds on Tron"));
        }
        if swap.call.value.decimals() != self.network.native.decimals {
            return Err(ChainError::Invalid(
                "the amount uses the wrong number of decimals",
            ));
        }
        let pays = match swap.sell.contract() {
            None => swap.call.value == swap.amount && swap.spender.is_none(),
            Some(token) => {
                swap.call.value.is_zero() && swap.spender.is_some() && *token != swap.call.to
            }
        };
        if !pays {
            return Err(ChainError::Invalid(
                "the swap does not pay for itself the way it claims",
            ));
        }
        Ok(())
    }

    fn approval(
        &self,
        swap: &ContractSwap,
        token: &[u8; 20],
        spender: &Address,
        units: U256,
    ) -> Result<(Step, Call, [u8; 20], i64), ChainError> {
        let mut data = APPROVE.to_vec();
        data.extend_from_slice(&[0u8; 12]);
        data.extend_from_slice(&bytes(spender)?);
        data.extend_from_slice(&units.to_be_bytes::<WORD>());
        let approved =
            Amount::new(units, swap.amount.decimals()).map_err(|_| ChainError::overflow())?;
        Ok((
            Step::Approve {
                asset: swap.sell.clone(),
                spender: *spender,
                amount: approved,
            },
            Call {
                to: Address::Tron(*token),
                value: amount(U256::ZERO, self.network.native.decimals)?,
                data,
            },
            *token,
            0,
        ))
    }

    async fn checked_call(
        &self,
        owner: &[u8; 20],
        contract: &[u8; 20],
        data: &[u8],
        value: i64,
        swap: &ContractSwap,
        last: bool,
    ) -> Result<u64, ChainError> {
        let reply = self
            .trigger(json!({
                "owner_address": tron_hex(owner),
                "contract_address": tron_hex(contract),
                "data": hex::encode(data),
                "call_value": value,
            }))
            .await
            .map_err(|err| match err {
                ChainError::Rejected(reason) => {
                    ChainError::Rejected(format!("the swap would fail: {reason}"))
                }
                other => other,
            })?;
        let energy = reply["energy_used"]
            .as_u64()
            .filter(|energy| *energy > 0)
            .ok_or_else(|| ChainError::Network("an endpoint sent no energy estimate".to_owned()))?;
        if let (true, Some((_, minimum))) = (last, &swap.buy) {
            let output = reply["constant_result"]
                .as_array()
                .and_then(|results| results.first())
                .and_then(|result| result.as_str())
                .and_then(hex::decode)
                .and_then(|data| last_amount(&data))
                .ok_or_else(ChainError::malformed)?;
            if output < minimum.units() {
                return Err(ChainError::Invalid(
                    "the swap would pay less than its minimum",
                ));
            }
        }
        Ok(energy)
    }
}

fn last_amount(data: &[u8]) -> Option<U256> {
    let word = |index: usize| -> Option<U256> {
        let start = index.checked_mul(WORD)?;
        Some(U256::from_be_slice(
            data.get(start..start.checked_add(WORD)?)?,
        ))
    };
    if word(0)? != U256::from(WORD) {
        return None;
    }
    let count = usize::try_from(word(1)?).ok().filter(|count| *count >= 2)?;
    if data.len() != (2 + count) * WORD {
        return None;
    }
    word(1 + count)
}

fn used(resources: Resources, bandwidth: u64, energy: u64) -> Resources {
    let staked = resources.staked_bandwidth >= bandwidth;
    Resources {
        free_bandwidth: if staked || resources.free_bandwidth < bandwidth {
            resources.free_bandwidth
        } else {
            resources.free_bandwidth - bandwidth
        },
        staked_bandwidth: if staked {
            resources.staked_bandwidth - bandwidth
        } else {
            resources.staked_bandwidth
        },
        energy: resources.energy.saturating_sub(energy),
    }
}

fn merge(total: &mut Quote, quote: Quote) -> Result<(), ChainError> {
    total.expected = total
        .expected
        .checked_add(quote.expected)
        .ok_or_else(ChainError::overflow)?;
    total.max = total
        .max
        .checked_add(quote.max)
        .ok_or_else(ChainError::overflow)?;
    for (item, sun) in quote.lines {
        let merged = total.lines.iter_mut().find(|(seen, _)| {
            matches!(
                (seen, &item),
                (FeeItem::Bandwidth { .. }, FeeItem::Bandwidth { .. })
                    | (FeeItem::Energy { .. }, FeeItem::Energy { .. })
            )
        });
        match merged {
            Some((seen, total)) => {
                *total = total.checked_add(sun).ok_or_else(ChainError::overflow)?;
                *seen = match (&*seen, &item) {
                    (FeeItem::Bandwidth { bytes: a }, FeeItem::Bandwidth { bytes: b }) => {
                        FeeItem::Bandwidth {
                            bytes: a.saturating_add(*b),
                        }
                    }
                    (FeeItem::Energy { units: a }, FeeItem::Energy { units: b }) => {
                        FeeItem::Energy {
                            units: a.saturating_add(*b),
                        }
                    }
                    _ => item,
                };
            }
            None => total.lines.push((item, sun)),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_router_answer_gives_the_last_amount_of_the_path() {
        let mut data = Vec::new();
        for value in [32u64, 2, 100_000_000, 32_197_142] {
            data.extend_from_slice(&U256::from(value).to_be_bytes::<WORD>());
        }
        assert_eq!(last_amount(&data), Some(U256::from(32_197_142u64)));
        assert_eq!(last_amount(&data[..96]), None);
    }

    #[test]
    fn a_signed_swap_call_reads_back_exactly() {
        use bahaa_chain::check_call;
        use bahaa_config::Registry;
        use bahaa_crypto::Mnemonic;

        let registry = Registry::builtin().unwrap();
        let chain =
            TronChain::new(registry.get(&"tron".parse().unwrap()).unwrap().clone()).unwrap();
        let seed = Mnemonic::parse("test test test test test test test test test test test junk")
            .unwrap()
            .to_seed("");
        let from = super::super::derive(&seed, 0).unwrap();
        let router = Address::Tron([0x7a; 20]);
        let call = Call {
            to: router,
            value: Amount::parse("10", 6).unwrap(),
            data: vec![0x7f, 0xf3, 0x6a, 0xb5, 1, 2, 3],
        };
        let anchor = tx::Anchor {
            number: 70_000_000,
            id: [3; 32],
            time_ms: 1_793_000_000_000,
        };
        let raw = tx::raw(
            tx::call(
                &bytes(&from).unwrap(),
                &[0x7a; 20],
                call.data.clone(),
                10_000_000,
            ),
            &anchor,
            30_000_000,
            None,
        )
        .unwrap();
        let plan = TronCall {
            planned: PlannedCall {
                step: Step::Swap,
                account: 0,
                from,
                call,
            },
            raw,
            fee_max: Amount::parse("30", 6).unwrap(),
            checked: true,
        };
        let signed = chain.sign_call(&plan, &seed).unwrap();
        check_call(plan.planned(), &chain.decode_call(&signed).unwrap()).unwrap();
        assert!(bahaa_chain::Chain::decode(&chain, &signed).is_err());
    }

    #[test]
    fn a_second_transaction_uses_what_the_first_left() {
        let left = used(
            Resources {
                free_bandwidth: 600,
                staked_bandwidth: 0,
                energy: 100_000,
            },
            345,
            60_000,
        );
        assert_eq!(left.free_bandwidth, 255);
        assert_eq!(left.energy, 40_000);
    }
}
