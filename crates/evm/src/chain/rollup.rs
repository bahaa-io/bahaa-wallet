use alloy_consensus::{SignableTransaction, TxEip1559};
use bahaa_chain::{ChainError, FeeItem};
use bahaa_config::{ChainKind, FeeModel};
use ruint::aliases::U256;
use serde_json::{Value, json};

use super::{EvmChain, Fees, hex, overflow};
use crate::rpc::quantity;

const ORACLE: [u8; 20] = [
    0x42, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x0f,
];
const GET_L1_FEE: [u8; 4] = [0x49, 0x94, 0x8e, 0x0e];
const GET_OPERATOR_FEE: [u8; 4] = [0x27, 0x5a, 0xed, 0xd2];
const L1_MARGIN: u8 = 2;

impl EvmChain {
    pub(super) fn is_rollup(&self) -> bool {
        matches!(
            self.network.kind,
            ChainKind::Evm {
                fee: FeeModel::OpStack,
                ..
            }
        )
    }

    pub(super) async fn add_rollup_fees(
        &self,
        tx: &TxEip1559,
        estimate: u64,
        fees: &mut Fees,
    ) -> Result<(), ChainError> {
        let latest = json!("latest");
        let l1 = self
            .oracle(&l1_fee_call(&tx.encoded_for_signing()), &latest)
            .await?;
        let l1_max = l1.checked_mul(U256::from(L1_MARGIN)).ok_or_else(overflow)?;
        fees.add(FeeItem::L1Data, l1, l1_max)?;
        let operator = self.oracle(&operator_fee_call(estimate), &latest).await?;
        let operator_max = self
            .oracle(&operator_fee_call(tx.gas_limit), &latest)
            .await?;
        if !operator_max.is_zero() {
            fees.add(FeeItem::Operator, operator, operator_max)?;
        }
        Ok(())
    }

    pub(super) async fn rollup_paid(&self, receipt: &Value, gas_used: U256) -> Option<U256> {
        let l1 = quantity(receipt.get("l1Fee")?).ok()?;
        let scalar = optional(receipt, "operatorFeeScalar")?;
        let constant = optional(receipt, "operatorFeeConstant")?;
        let operator = if scalar.is_zero() && constant.is_zero() {
            U256::ZERO
        } else {
            let gas = u64::try_from(gas_used).ok()?;
            self.oracle(&operator_fee_call(gas), &receipt["blockNumber"])
                .await
                .ok()?
        };
        l1.checked_add(operator)
    }

    async fn oracle(&self, data: &[u8], block: &Value) -> Result<U256, ChainError> {
        let reply = self
            .rpc
            .call(
                "eth_call",
                json!([{"to": hex(&ORACLE), "data": hex(data)}, block]),
            )
            .await?;
        if reply.as_str() == Some("0x") {
            return Err(ChainError::Unsupported("the OP Stack fee oracle"));
        }
        quantity(&reply)
    }
}

fn optional(receipt: &Value, field: &str) -> Option<U256> {
    match receipt.get(field) {
        None | Some(Value::Null) => Some(U256::ZERO),
        Some(value) => quantity(value).ok(),
    }
}

fn l1_fee_call(unsigned: &[u8]) -> Vec<u8> {
    let mut data = GET_L1_FEE.to_vec();
    data.extend_from_slice(&U256::from(32u8).to_be_bytes::<32>());
    data.extend_from_slice(&U256::from(unsigned.len()).to_be_bytes::<32>());
    data.extend_from_slice(unsigned);
    data.resize(GET_L1_FEE.len() + 64 + unsigned.len().div_ceil(32) * 32, 0);
    data
}

fn operator_fee_call(gas: u64) -> Vec<u8> {
    let mut data = GET_OPERATOR_FEE.to_vec();
    data.extend_from_slice(&U256::from(gas).to_be_bytes::<32>());
    data
}

#[cfg(test)]
mod tests {
    use super::*;

    const UNSIGNED: &str = "02f86b822105808405f5e10084b2d05e0082520894f39fd6e51aad88f6f4ce6ab8827279cfffb92266880de0b6b3a764000080c0";

    fn unhex(digits: &str) -> Vec<u8> {
        (0..digits.len())
            .step_by(2)
            .map(|at| u8::from_str_radix(&digits[at..at + 2], 16).unwrap())
            .collect()
    }

    #[test]
    fn oracle_calls_match_the_solidity_abi() {
        assert_eq!(
            hex(&l1_fee_call(&unhex(UNSIGNED))),
            format!(
                "0x49948e0e{:064x}{:064x}{UNSIGNED}{}",
                0x20,
                0x34,
                "00".repeat(12)
            )
        );
        assert_eq!(l1_fee_call(&[7; 32]).len(), 4 + 32 * 3);
        assert_eq!(
            hex(&operator_fee_call(21_000)),
            "0x275aedd20000000000000000000000000000000000000000000000000000000000005208"
        );
    }

    #[test]
    fn missing_operator_fields_mean_no_operator_fee() {
        let receipt = json!({"operatorFeeScalar": "0x3", "operatorFeeConstant": null});
        assert_eq!(
            optional(&receipt, "operatorFeeScalar"),
            Some(U256::from(3u8))
        );
        assert_eq!(optional(&receipt, "operatorFeeConstant"), Some(U256::ZERO));
        assert_eq!(optional(&receipt, "absent"), Some(U256::ZERO));
        assert_eq!(optional(&json!({"x": "3"}), "x"), None);
    }
}
