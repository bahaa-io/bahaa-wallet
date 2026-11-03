use bahaa_chain::{
    Call, ChainError, ContractSwap, DecodedCall, FeeQuote, Pace, PlannedCall, Signed, Step,
    Transfer, enough,
};
use bahaa_crypto::Seed;
use bahaa_types::Amount;
use ruint::aliases::U256;

use super::send::{CONFIRM_BLOCKS, LIFETIME, TonPlan, estimate, fee_lines, prices};
use super::{TonChain, amount};
use crate::body::read_jetton_amount;
use crate::wallet::{self, Order};

const MAX_GAS: u64 = 1_000_000_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TonCall {
    planned: PlannedCall,
    plan: TonPlan,
    fee_max: Amount,
}

impl TonCall {
    pub fn planned(&self) -> &PlannedCall {
        &self.planned
    }

    pub fn fee_max(&self) -> Amount {
        self.fee_max
    }
}

impl TonChain {
    pub async fn jetton_wallet_of(
        &self,
        owner: &bahaa_types::Address,
        master: &bahaa_types::Address,
    ) -> Result<bahaa_types::Address, ChainError> {
        self.jetton_wallet(owner, master).await
    }

    pub async fn prepare_swap(
        &self,
        swap: &ContractSwap,
    ) -> Result<(Vec<TonCall>, FeeQuote), ChainError> {
        let gas = self.checked_swap(swap).await?;
        let wallet = self.http.account(&swap.from).await?;
        let (seqno, deploy, contract) = self.sequence(&wallet, &swap.from)?;
        let now = bahaa_chain::now();
        let clock =
            u32::try_from(now).map_err(|_| ChainError::Invalid("the clock is out of range"))?;
        let config = self.http.config().await?;
        let (settings, storage) = prices(&config)?;
        let order = Order {
            wallet: swap.from,
            contract,
            wallet_id: self.subwallet(contract),
            seqno,
            valid_until: u32::try_from(now + LIFETIME)
                .map_err(|_| ChainError::Invalid("the clock is out of range"))?,
            to: swap.call.to,
            nano: swap.call.value.units(),
            everything: false,
            bounce: true,
            token: None,
            memo: None,
            call: Some(swap.call.data.clone()),
        };
        let fees = estimate(&order, deploy, &wallet, (&settings, &storage), clock)?;
        let own = fees.expected().ok_or_else(ChainError::overflow)?;
        let own_max = own.checked_mul(2).ok_or_else(ChainError::overflow)?;
        let spent = match swap.spent {
            Some(spent) => u64::try_from(spent.units())
                .ok()
                .filter(|spent| *spent <= gas)
                .ok_or(ChainError::Invalid(
                    "the swap expects to keep more than it attaches",
                ))?,
            None => gas,
        };
        let needed = swap
            .call
            .value
            .units()
            .checked_add(U256::from(own_max))
            .ok_or_else(ChainError::overflow)?;
        let native = &self.network.native;
        enough(&native.symbol, needed, wallet.balance, native.decimals)?;
        let gram = |nano: u64| amount(U256::from(nano), native.decimals);
        let expected = gram(own.checked_add(spent).ok_or_else(ChainError::overflow)?)?;
        let max = gram(own_max.checked_add(gas).ok_or_else(ChainError::overflow)?)?;
        let lines = fee_lines(&fees, contract, Some(spent), gram)?;
        let plan = TonPlan {
            transfer: Transfer {
                account: swap.account,
                from: swap.from,
                to: swap.call.to,
                asset: swap.sell.clone(),
                amount: swap.amount,
                max: false,
                memo: None,
            },
            order,
            deploy,
        };
        let call = TonCall {
            planned: PlannedCall {
                step: Step::Swap,
                account: swap.account,
                from: swap.from,
                call: swap.call.clone(),
            },
            plan,
            fee_max: max,
        };
        Ok((
            vec![call],
            FeeQuote::new(max, expected, lines)?
                .paced(Pace::blocks(CONFIRM_BLOCKS, self.network.block_ms)),
        ))
    }

    pub async fn recheck(&self, swap: &ContractSwap) -> Result<(), ChainError> {
        self.checked_swap(swap).await.map(|_| ())
    }

    pub fn sign_call(&self, plan: &TonCall, seed: &Seed) -> Result<Signed, ChainError> {
        self.sign_plan(&plan.plan, seed)
    }

    pub fn decode_call(&self, signed: &Signed) -> Result<DecodedCall, ChainError> {
        let parsed = wallet::parse_call(&signed.bytes)?;
        self.verified(&parsed, signed)?;
        let order = parsed.order;
        Ok(DecodedCall {
            from: order.wallet,
            call: Call {
                to: order.to,
                value: amount(order.nano, self.network.native.decimals)?,
                data: order.call.ok_or(ChainError::Invalid("not a call"))?,
            },
            sequence: Some(u64::from(order.seqno)),
        })
    }

    async fn checked_swap(&self, swap: &ContractSwap) -> Result<u64, ChainError> {
        let network = &self.network.id;
        let elsewhere = swap.sell.network() != network
            || swap
                .buy
                .as_ref()
                .is_some_and(|(asset, _)| asset.network() != network);
        if elsewhere || swap.amount.is_zero() || swap.spender.is_some() || !swap.ours {
            return Err(ChainError::Invalid("not a swap this wallet builds on TON"));
        }
        crate::cells::parts(&swap.from)?;
        crate::cells::parts(&swap.call.to)?;
        crate::cells::bag(&swap.call.data)
            .and_then(|bag| bag.single_root().ok())
            .ok_or(ChainError::Invalid("the swap message can't be read"))?;
        if swap.call.value.decimals() != self.network.native.decimals {
            return Err(ChainError::Invalid(
                "the amount uses the wrong number of decimals",
            ));
        }
        let value = swap.call.value.units();
        let gas = match swap.sell.contract() {
            None => value.checked_sub(swap.amount.units()),
            Some(master) => {
                let own = self.jetton_wallet(&swap.from, master).await?;
                let exact = swap.call.to == own
                    && read_jetton_amount(&swap.call.data) == Some(swap.amount.units());
                if !exact {
                    return Err(ChainError::Invalid(
                        "the swap would take another amount than the one shown",
                    ));
                }
                let held = self.jetton_balance(&swap.from, master).await?;
                enough(
                    &self.symbol(master),
                    swap.amount.units(),
                    held,
                    swap.amount.decimals(),
                )?;
                Some(value)
            }
        };
        gas.and_then(|gas| u64::try_from(gas).ok())
            .filter(|gas| *gas <= MAX_GAS)
            .ok_or(ChainError::Invalid(
                "the swap attaches more network coin than a swap needs",
            ))
    }
}

#[cfg(test)]
mod tests {
    use bahaa_chain::{Chain, check_call};
    use bahaa_config::Registry;
    use bahaa_crypto::Mnemonic;
    use bahaa_types::{Address, TonContract};

    use super::*;
    use crate::body::Body;

    fn signed_call(contract: TonContract, deploy: bool) -> (TonChain, TonCall) {
        let registry = Registry::builtin().unwrap();
        let chain = TonChain::new(registry.get(&"ton".parse().unwrap()).unwrap().clone()).unwrap();
        let seed = Mnemonic::parse("test test test test test test test test test test test junk")
            .unwrap()
            .to_seed("");
        let from = chain.derive_as(&seed, 0, contract).unwrap();
        let router = Address::Ton {
            workchain: 0,
            hash: [0x3a; 32],
        };
        let body = Body::default()
            .uint(32, 0x0f8a_7ea5)
            .unwrap()
            .uint(64, 7)
            .unwrap()
            .coins(U256::from(1_000_000u32))
            .unwrap()
            .address(Some(&router))
            .unwrap()
            .address(Some(&from))
            .unwrap()
            .bit(false)
            .unwrap()
            .coins(U256::from(240_000_000u32))
            .unwrap()
            .bit(true)
            .unwrap()
            .reference(Body::default().uint(32, 0x6664_de2a).unwrap())
            .unwrap()
            .boc()
            .unwrap();
        let call = Call {
            to: router,
            value: Amount::parse("0.3", 9).unwrap(),
            data: body.clone(),
        };
        let order = Order {
            wallet: from,
            contract,
            wallet_id: chain.subwallet(contract),
            seqno: if deploy { 0 } else { 3 },
            valid_until: 1_792_400_000,
            to: router,
            nano: call.value.units(),
            everything: false,
            bounce: true,
            token: None,
            memo: None,
            call: Some(body),
        };
        let planned = PlannedCall {
            step: Step::Swap,
            account: 0,
            from,
            call,
        };
        let plan = TonPlan {
            transfer: Transfer {
                account: 0,
                from,
                to: router,
                asset: "ton/EQCxE6mUtQJKFnGfaROTKOt1lZbDiiX1kCixRv7Nw2Id_sDs"
                    .parse()
                    .unwrap(),
                amount: Amount::parse("1", 6).unwrap(),
                max: false,
                memo: None,
            },
            order,
            deploy,
        };
        let fee_max = Amount::parse("0.31", 9).unwrap();
        let ton_call = TonCall {
            planned,
            plan,
            fee_max,
        };
        let signed = chain.sign_call(&ton_call, &seed).unwrap();
        let decoded = chain.decode_call(&signed).unwrap();
        check_call(ton_call.planned(), &decoded).unwrap();
        assert_eq!(decoded.sequence, Some(if deploy { 0 } else { 3 }));
        assert!(
            chain.decode(&signed).is_err(),
            "a call is not read as a plain transfer"
        );
        (chain, ton_call)
    }

    #[test]
    fn a_signed_swap_message_reads_back_exactly_on_both_wallets() {
        for contract in [TonContract::W5, TonContract::V4R2] {
            for deploy in [false, true] {
                signed_call(contract, deploy);
            }
        }
    }

    #[test]
    fn the_amount_inside_a_jetton_transfer_is_read_strictly() {
        let (_, call) = signed_call(TonContract::W5, false);
        assert_eq!(
            read_jetton_amount(&call.planned().call.data),
            Some(U256::from(1_000_000u32))
        );
        let other = Body::default().uint(32, 1).unwrap().boc().unwrap();
        assert_eq!(read_jetton_amount(&other), None);
    }
}
