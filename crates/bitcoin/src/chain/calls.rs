use std::cmp::Reverse;

use bahaa_chain::{
    Call, ChainError, ContractSwap, DecodedCall, FeeItem, FeeLine, FeeQuote, PlannedCall, Signed,
    Step, Transfer,
};
use bahaa_crypto::Seed;
use bahaa_types::Amount;
use futures_util::future::try_join;
use ruint::aliases::U256;

use super::BitcoinChain;
use super::account::{Coin, Funds, script};
use super::send::{BitcoinPlan, conflict, cost, ordered};
use crate::script::{dust, locking};
use crate::tx::Output;

const OP_RETURN: u8 = 0x6a;
const PUSH_MAX: u8 = 0x4b;
const PUSH_ONE: u8 = 0x4c;
const MAX_NULLDATA: usize = 80;

#[derive(Clone, Debug)]
pub struct BitcoinCall {
    planned: PlannedCall,
    plan: BitcoinPlan,
    fee_max: Amount,
}

impl BitcoinCall {
    pub fn planned(&self) -> &PlannedCall {
        &self.planned
    }

    pub fn fee_max(&self) -> Amount {
        self.fee_max
    }
}

struct Picked {
    coins: Vec<Coin>,
    size: u64,
    paid: u64,
    refund: u64,
}

impl BitcoinChain {
    pub async fn prepare_swap(
        &self,
        swap: &ContractSwap,
    ) -> Result<(Vec<BitcoinCall>, FeeQuote), ChainError> {
        self.check_swap(swap)?;
        let vault = self.recipient(&swap.call.to)?;
        let units = u64::try_from(swap.amount.units()).map_err(|_| ChainError::overflow())?;
        if units < dust(script(&swap.call.to)?) {
            return Err(ChainError::Invalid(
                "this amount is below the smallest output Bitcoin relays",
            ));
        }
        let (funds, estimates) = try_join(self.funds(&swap.from), self.sources.estimates()).await?;
        if self.takes_change(&swap.from, &swap.call.to) {
            return Err(ChainError::Invalid(
                "a swap deposit can't go to this wallet's own address",
            ));
        }
        let rate = estimates.rate();
        let refund_script = locking(script(&funds.change)?);
        let mut shape = vec![
            Output {
                value: units,
                script: vault.clone(),
            },
            Output {
                value: 0,
                script: nulldata(&swap.call.data)?,
            },
            Output {
                value: 0,
                script: refund_script,
            },
        ];
        let least = dust(script(&funds.change)?);
        let picked = pick(&funds, &shape, units, least, rate).map_err(|needed| {
            ChainError::insufficient(
                self.network.native.symbol.clone(),
                U256::from(needed),
                U256::from(funds.total().unwrap_or(u64::MAX)),
                self.network.native.decimals,
            )
        })?;
        shape[2].value = picked.refund;
        let (spent, tx) = ordered(&picked.coins, shape);
        let fee = self.sats(picked.paid)?;
        let plan = BitcoinPlan {
            transfer: Transfer {
                account: swap.account,
                from: swap.from,
                to: swap.call.to,
                asset: swap.sell.clone(),
                amount: swap.amount,
                max: false,
                memo: None,
            },
            tx,
            spent,
        };
        let lines = vec![FeeLine {
            item: FeeItem::Size {
                vbytes: picked.size,
            },
            amount: fee,
        }];
        let call = BitcoinCall {
            planned: PlannedCall {
                step: Step::Swap,
                account: swap.account,
                from: swap.from,
                call: swap.call.clone(),
            },
            plan,
            fee_max: fee,
        };
        Ok((
            vec![call],
            FeeQuote::new(fee, fee, lines)?.paced(self.pace(&estimates, rate)),
        ))
    }

    pub fn recheck(&self, swap: &ContractSwap) -> Result<(), ChainError> {
        self.check_swap(swap)
    }

    pub fn sign_call(&self, plan: &BitcoinCall, seed: &Seed) -> Result<Signed, ChainError> {
        self.signed(&plan.plan, seed)
    }

    pub fn decode_call(&self, signed: &Signed) -> Result<DecodedCall, ChainError> {
        let unusual = || ChainError::Invalid("this is not a swap deposit this wallet makes");
        let (tx, from) = self.verified(signed)?;
        let [deposit, data, refund] = tx.outputs.as_slice() else {
            return Err(unusual());
        };
        let vault = self.address(&deposit.script).ok_or_else(unusual)?;
        let payload = read_nulldata(&data.script).ok_or_else(unusual)?;
        let refunded = self
            .address(&refund.script)
            .is_some_and(|address| self.takes_change(&from, &address));
        if data.value != 0 || !refunded || self.takes_change(&from, &vault) {
            return Err(unusual());
        }
        Ok(DecodedCall {
            from,
            call: Call {
                to: vault,
                value: self.sats(deposit.value)?,
                data: payload,
            },
            sequence: tx.inputs.first().map(|input| conflict(&input.previous)),
        })
    }

    fn check_swap(&self, swap: &ContractSwap) -> Result<(), ChainError> {
        self.native(std::slice::from_ref(&swap.sell))?;
        let plain = swap.spender.is_none()
            && swap.buy.is_none()
            && swap.ours
            && swap.call.value == swap.amount
            && !swap.call.data.is_empty()
            && swap.call.data.len() <= MAX_NULLDATA;
        if !plain {
            return Err(ChainError::Invalid(
                "not a swap deposit this wallet builds on Bitcoin",
            ));
        }
        Ok(())
    }
}

fn pick(
    funds: &Funds,
    shape: &[Output],
    amount: u64,
    least: u64,
    rate: u64,
) -> Result<Picked, u64> {
    let mut ordered: Vec<Coin> = funds.coins.clone();
    ordered.sort_by_key(|coin| (coin.height <= 0, Reverse(coin.value)));
    let mut chosen = Vec::new();
    let mut held = 0u64;
    for coin in &ordered {
        chosen.push(coin.clone());
        held = held.saturating_add(coin.value);
        let (size, paid) = cost(&chosen, shape, rate);
        if let Some(refund) = held
            .checked_sub(amount)
            .and_then(|rest| rest.checked_sub(paid))
            .filter(|refund| *refund >= least)
        {
            return Ok(Picked {
                coins: chosen,
                size,
                paid,
                refund,
            });
        }
    }
    let (_, paid) = cost(&ordered, shape, rate);
    Err(amount.saturating_add(paid).saturating_add(least))
}

fn nulldata(payload: &[u8]) -> Result<Vec<u8>, ChainError> {
    let length = u8::try_from(payload.len())
        .ok()
        .filter(|length| *length > 0 && usize::from(*length) <= MAX_NULLDATA)
        .ok_or(ChainError::Invalid(
            "the swap data does not fit in a Bitcoin output",
        ))?;
    let mut script = vec![OP_RETURN];
    if length > PUSH_MAX {
        script.push(PUSH_ONE);
    }
    script.push(length);
    script.extend_from_slice(payload);
    Ok(script)
}

fn read_nulldata(script: &[u8]) -> Option<Vec<u8>> {
    let (&OP_RETURN, rest) = script.split_first()? else {
        return None;
    };
    let (length, data) = match rest.split_first()? {
        (&PUSH_ONE, rest) => rest
            .split_first()
            .filter(|(length, _)| **length > PUSH_MAX)?,
        (length, data) if (1..=PUSH_MAX).contains(length) => (length, data),
        _ => return None,
    };
    let length = *length;
    let payload = data.to_vec();
    (payload.len() == usize::from(length) && payload.len() <= MAX_NULLDATA).then_some(payload)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nulldata_uses_the_shortest_push_and_reads_back_strictly() {
        let short = nulldata(&[7; 48]).unwrap();
        assert_eq!(short[..2], [OP_RETURN, 48]);
        assert_eq!(read_nulldata(&short), Some(vec![7; 48]));
        let long = nulldata(&[7; 80]).unwrap();
        assert_eq!(long[..3], [OP_RETURN, PUSH_ONE, 80]);
        assert_eq!(read_nulldata(&long), Some(vec![7; 80]));
        assert!(nulldata(&[7; 81]).is_err());
        assert!(nulldata(&[]).is_err());
        let mut padded = short.clone();
        padded.push(0);
        assert_eq!(read_nulldata(&padded), None);
        let wasteful = [OP_RETURN, PUSH_ONE, 48]
            .into_iter()
            .chain([7; 48])
            .collect::<Vec<_>>();
        assert_eq!(read_nulldata(&wasteful), None);
    }
}
