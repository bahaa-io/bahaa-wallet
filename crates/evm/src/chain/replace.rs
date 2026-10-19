use alloy_consensus::TxEip1559;
use alloy_primitives::TxKind;
use bahaa_chain::{Chain, ChainError, FeeQuote, Replace, Signed, Transfer};
use bahaa_types::{Address, Amount, AssetId};
use ruint::aliases::U256;

use super::{EvmChain, EvmPlan, PLAIN_GAS, Route, bytes};
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
        let (transfer, route, gas_limit) = match how {
            Replace::SpeedUp => {
                let TxKind::Call(target) = old.tx.to else {
                    return Err(ChainError::Invalid("contract creation is not a transfer"));
                };
                let token = decoded.asset.contract().map(bytes).transpose()?;
                let route = Route {
                    call_to: target.into_array(),
                    value: old.tx.value,
                    data: old.tx.input.to_vec(),
                    token,
                    decimals: decoded.amount.decimals(),
                };
                let transfer = Transfer {
                    account,
                    from: decoded.from,
                    to: decoded.to,
                    asset: decoded.asset,
                    amount: decoded.amount,
                    max: false,
                    memo: decoded.memo,
                };
                (transfer, route, old.tx.gas_limit)
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
                (transfer, route, PLAIN_GAS)
            }
        };
        let estimate = match how {
            Replace::SpeedUp => self.estimate(&from, &route).await?.min(gas_limit),
            Replace::Cancel => PLAIN_GAS,
        };
        let (base, tip) = self.fees().await?;
        let tip = tip.max(bumped(old.tx.max_priority_fee_per_gas)?);
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
            self.fee_quote(fees)?,
        ))
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
