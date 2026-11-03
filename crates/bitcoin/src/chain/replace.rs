use std::collections::HashMap;

use bahaa_chain::{ChainError, FeeItem, FeeLine, FeeQuote, Replace, Replaced, Signed, Transfer};
use bahaa_types::{Address, AssetId};

use super::account::{Coin, script};
use super::send::{BitcoinPlan, cost, unsigned};
use super::{BitcoinChain, spent};
use crate::script::{dust, locking};
use crate::tx::{Output, Tx, parse};

const BUMP: (u64, u64) = (5, 4);
const STEP_PER_KVB: u64 = 1_000;

struct Bump {
    coins: Vec<Coin>,
    total: u64,
    old_fee: u64,
    rate: u64,
}

impl Bump {
    fn fee(&self, outputs: &[Output]) -> (u64, u64) {
        let (size, fee) = cost(&self.coins, outputs, self.rate);
        (size, fee.max(self.old_fee.saturating_add(size)))
    }
}

impl BitcoinChain {
    pub(super) async fn replacement(
        &self,
        original: &Signed,
        account: u32,
        how: Replace,
    ) -> Result<(BitcoinPlan, FeeQuote), ChainError> {
        let old = parse(&original.bytes)?;
        let (owner, coins) = self.spent_coins(&old).await?;
        let total = sum(coins.iter().map(|coin| coin.value))?;
        let old_fee = total
            .checked_sub(sum(old.outputs.iter().map(|output| output.value))?)
            .ok_or_else(ChainError::malformed)?;
        let old_rate = old_fee.saturating_mul(1_000) / old.vsize().max(1);
        let estimates = self.sources.estimates().await?;
        let rate = estimates
            .rate()
            .max(old_rate.saturating_mul(BUMP.0) / BUMP.1)
            .max(old_rate.saturating_add(STEP_PER_KVB));
        let bump = Bump {
            coins,
            total,
            old_fee,
            rate,
        };
        let (outputs, to) = match how {
            Replace::SpeedUp => self.faster(&old, &owner, &bump)?,
            Replace::Cancel => {
                let back = self.used(&owner).await?.change;
                (cancelled(&bump, &back)?, back)
            }
        };
        let (size, fee) = bump.fee(&outputs);
        let paid = total
            .checked_sub(sum(outputs.iter().map(|output| output.value))?)
            .ok_or_else(ChainError::malformed)?;
        let leftover = paid.saturating_sub(fee);
        let sent = outputs
            .iter()
            .find(|output| self.address(&output.script) == Some(to))
            .map_or(0, |output| output.value);
        let transfer = Transfer {
            account,
            from: owner,
            to,
            asset: AssetId::Native(self.network.id.clone()),
            amount: self.sats(sent)?,
            max: false,
            memo: None,
        };
        let mut lines = vec![FeeLine {
            item: FeeItem::Size { vbytes: size },
            amount: self.sats(paid - leftover)?,
        }];
        if leftover > 0 {
            lines.push(FeeLine {
                item: FeeItem::Leftover,
                amount: self.sats(leftover)?,
            });
        }
        let quoted = self.sats(paid)?;
        let before = Replaced {
            fee: self.sats(old_fee)?,
            pace: self.pace(&estimates, old_rate),
        };
        let (inputs, tx) = unsigned(&bump.coins, outputs);
        Ok((
            BitcoinPlan {
                transfer,
                tx,
                spent: inputs,
            },
            FeeQuote::new(quoted, quoted, lines)?
                .paced(self.pace(&estimates, rate))
                .replacing(before),
        ))
    }

    fn faster(
        &self,
        old: &Tx,
        owner: &Address,
        bump: &Bump,
    ) -> Result<(Vec<Output>, Address), ChainError> {
        let short = || ChainError::Invalid("too little is left over to pay a higher fee");
        let mut outputs = old.outputs.clone();
        let addresses: Vec<Option<Address>> = outputs
            .iter()
            .map(|output| self.address(&output.script))
            .collect();
        let change = addresses
            .iter()
            .position(|address| address.is_some_and(|address| self.takes_change(owner, &address)));
        let (paid_at, recipient) = addresses
            .iter()
            .enumerate()
            .find_map(|(at, address)| (Some(at) != change).then_some((at, (*address)?)))
            .ok_or(ChainError::Invalid(
                "this is not a transfer this wallet makes",
            ))?;
        let (_, fee) = bump.fee(&outputs);
        let extra = fee.saturating_sub(bump.old_fee);
        if let Some(at) = change {
            let floor = addresses[at]
                .as_ref()
                .map(script)
                .transpose()?
                .map_or(0, dust);
            if outputs[at].value >= extra.saturating_add(floor) {
                outputs[at].value -= extra;
            } else {
                let freed = outputs.remove(at).value;
                let (_, fee) = bump.fee(&outputs);
                if bump.old_fee.saturating_add(freed) < fee {
                    return Err(short());
                }
            }
        } else {
            let floor = dust(script(&recipient)?);
            outputs[paid_at].value = outputs[paid_at]
                .value
                .checked_sub(extra)
                .filter(|value| *value >= floor)
                .ok_or_else(short)?;
        }
        Ok((outputs, recipient))
    }

    async fn spent_coins(&self, old: &Tx) -> Result<(Address, Vec<Coin>), ChainError> {
        let mut known = HashMap::new();
        self.load(
            &mut known,
            old.inputs.iter().map(|input| input.previous.txid),
        )
        .await?;
        let mut owner = None;
        let mut coins = Vec::with_capacity(old.inputs.len());
        for input in &old.inputs {
            let output = spent(&known, &input.previous).ok_or_else(ChainError::malformed)?;
            let address = self
                .address(&output.script)
                .ok_or_else(ChainError::malformed)?;
            let mine = self
                .book()
                .place(&address)
                .map_or(address, |place| place.owner);
            if owner.replace(mine).is_some_and(|before| before != mine) {
                return Err(ChainError::Mismatch("the sender"));
            }
            coins.push(Coin {
                outpoint: input.previous,
                value: output.value,
                height: 0,
                address,
            });
        }
        self.book().remember(&coins);
        Ok((owner.ok_or_else(ChainError::malformed)?, coins))
    }
}

fn cancelled(bump: &Bump, back: &Address) -> Result<Vec<Output>, ChainError> {
    let kind = script(back)?;
    let mut output = Output {
        value: 0,
        script: locking(kind),
    };
    let (_, fee) = bump.fee(std::slice::from_ref(&output));
    output.value = bump
        .total
        .checked_sub(fee)
        .filter(|value| *value >= dust(kind))
        .ok_or(ChainError::Invalid(
            "too little is left over to pay a higher fee",
        ))?;
    Ok(vec![output])
}

fn sum(mut values: impl Iterator<Item = u64>) -> Result<u64, ChainError> {
    values
        .try_fold(0u64, u64::checked_add)
        .ok_or_else(ChainError::overflow)
}
