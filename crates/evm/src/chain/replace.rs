use alloy_consensus::TxEip1559;
use alloy_primitives::TxKind;
use bahaa_chain::{Chain, ChainError, FeeQuote, Pace, Replace, Replaced, Signed, Transfer, Wait};
use bahaa_types::{Address, Amount, AssetId};
use ruint::aliases::U256;

use super::{EvmChain, EvmPlan, PLAIN_GAS, Route, bytes, gwei};
use crate::tx;

const BUMP: (u128, u128) = (9, 8);

impl EvmChain {
    pub(super) async fn replacement(
        &self,
        original: &Signed,
        account: u32,
        how: Replace,
    ) -> Result<(EvmPlan, FeeQuote), ChainError> {
        let old = tx::parse(&original.bytes)?;
        if old.tx.chain_id != self.chain_id {
            return Err(ChainError::Mismatch("the network"));
        }
        let decoded = self.decode(original)?;
        let from = old.from;
        let native = AssetId::Native(self.network.id.clone());
        let TxKind::Call(target) = old.tx.to else {
            return Err(ChainError::Invalid("contract creation is not a transfer"));
        };
        let original_route = Route {
            call_to: target.into_array(),
            value: old.tx.value,
            data: old.tx.input.to_vec(),
            token: decoded.asset.contract().map(bytes).transpose()?,
            decimals: decoded.amount.decimals(),
        };
        let used = self
            .estimate(&from, &original_route)
            .await
            .map(|gas| gas.min(old.tx.gas_limit));
        let (transfer, route, gas_limit, estimate, used) = match how {
            Replace::SpeedUp => {
                let transfer = Transfer {
                    account,
                    from: decoded.from,
                    to: decoded.to,
                    asset: decoded.asset,
                    amount: decoded.amount,
                    max: false,
                    memo: decoded.memo,
                };
                let used = used?;
                (transfer, original_route, old.tx.gas_limit, used, used)
            }
            Replace::Cancel => {
                let route = Route {
                    call_to: from,
                    value: U256::ZERO,
                    data: Vec::new(),
                    token: None,
                    decimals: self.network.native.decimals,
                };
                let transfer = Transfer {
                    account,
                    from: Address::Evm(from),
                    to: Address::Evm(from),
                    asset: native,
                    amount: Amount::zero(self.network.native.decimals)
                        .map_err(|_| ChainError::overflow())?,
                    max: false,
                    memo: None,
                };
                (
                    transfer,
                    route,
                    PLAIN_GAS,
                    PLAIN_GAS,
                    used.unwrap_or(old.tx.gas_limit),
                )
            }
        };
        let (base, market_tip) = self.fees().await?;
        let before = self.before(&old.tx, used, base, market_tip)?;
        let tip = market_tip.max(bumped(old.tx.max_priority_fee_per_gas)?);
        let max_fee = base
            .checked_mul(U256::from(2u8))
            .and_then(|fee| fee.checked_add(tip))
            .ok_or_else(ChainError::overflow)?
            .max(bumped(old.tx.max_fee_per_gas)?);
        let mut fees = self.priced((estimate, gas_limit), base, tip, max_fee)?;
        let replacement = TxEip1559 {
            chain_id: self.chain_id,
            nonce: old.tx.nonce,
            gas_limit,
            max_fee_per_gas: u128::try_from(fees.max_fee).map_err(|_| ChainError::overflow())?,
            max_priority_fee_per_gas: u128::try_from(fees.tip)
                .map_err(|_| ChainError::overflow())?,
            to: TxKind::Call(route.call_to.into()),
            value: route.value,
            input: route.data.clone().into(),
            ..TxEip1559::default()
        };
        if self.is_rollup() {
            self.add_rollup_fees(&replacement, estimate, &mut fees)
                .await?;
        }
        let held = self.held(&from, route.token.as_ref()).await?;
        self.funds(&held, &transfer, &route, fees.max)?;
        Ok((
            EvmPlan {
                transfer,
                tx: replacement,
            },
            self.fee_quote(fees)?.replacing(before),
        ))
    }
}

impl EvmChain {
    fn before(
        &self,
        old: &TxEip1559,
        estimate: u64,
        base: U256,
        market_tip: U256,
    ) -> Result<Replaced, ChainError> {
        let old_max = U256::from(old.max_fee_per_gas);
        let old_tip = U256::from(old.max_priority_fee_per_gas);
        let price = base
            .checked_add(old_tip)
            .ok_or_else(ChainError::overflow)?
            .min(old_max);
        let wait = if old_max < base {
            Wait::Stuck
        } else if old_tip < market_tip {
            Wait::Slow
        } else {
            Wait::blocks(1, self.network.block_ms)
        };
        Ok(Replaced {
            fee: self.native(
                U256::from(estimate)
                    .checked_mul(price)
                    .ok_or_else(ChainError::overflow)?,
            )?,
            pace: Pace {
                rate: gwei(old_max),
                wait: Some(wait),
            },
        })
    }
}

fn bumped(old: u128) -> Result<U256, ChainError> {
    let raised = old
        .checked_mul(BUMP.0)
        .map(|scaled| scaled / BUMP.1)
        .and_then(|raised| raised.checked_add(1))
        .ok_or_else(ChainError::overflow)?;
    Ok(U256::from(raised))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_replacement_pays_at_least_an_eighth_more() {
        assert_eq!(bumped(800).unwrap(), U256::from(901u32));
        assert_eq!(bumped(0).unwrap(), U256::from(1u8));
        assert_eq!(bumped(50_000_000).unwrap(), U256::from(56_250_001u64));
        assert!(bumped(u128::MAX).is_err());
    }
}
