use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};

use alloy_consensus::TxEip1559;
use alloy_primitives::TxKind;
use bahaa_chain::{
    Balance, Chain, ChainError, Decoded, Failure, FeeItem, FeeLine, FeeQuote, MAX_MEMO, Replace,
    Signed, Transfer, TxId, TxStatus, abi_text, amount, parse_transfer, transfer_data,
};
use bahaa_config::{ChainKind, Coin, Network};
use bahaa_crypto::{DerivationPath, Seed, secp256k1_address};
use bahaa_types::{Address, Amount, AssetId, MAX_DECIMALS, is_plain_text};
use futures_util::future::{try_join, try_join_all, try_join3};
use ruint::aliases::U256;
use serde_json::{Value, json};

mod logs;
mod replace;
mod rollup;

use crate::rpc::{Rpc, quantity};
use crate::tx;

const BALANCE_OF: &str = "70a08231";
const DECIMALS: &str = "313ce567";
const SYMBOL: &str = "95d89b41";
const NAME: &str = "06fdde03";
const PLAIN_GAS: u64 = 21_000;

#[derive(Debug)]
pub struct EvmChain {
    network: Network,
    chain_id: u64,
    rpc: Rpc,
    decimals: Mutex<HashMap<[u8; 20], u8>>,
}

struct Held {
    native: U256,
    token: U256,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EvmPlan {
    transfer: Transfer,
    tx: TxEip1559,
}

impl EvmChain {
    pub fn new(network: Network) -> Result<Self, ChainError> {
        let ChainKind::Evm { chain_id, .. } = network.kind else {
            return Err(ChainError::Invalid("not an EVM network"));
        };
        let rpc = Rpc::new(network.endpoints.clone(), chain_id)?;
        Ok(Self {
            network,
            chain_id,
            rpc,
            decimals: Mutex::new(HashMap::new()),
        })
    }

    fn native(&self, units: U256) -> Result<Amount, ChainError> {
        amount(units, self.network.native.decimals)
    }

    fn known_decimals(&self, contract: &[u8; 20]) -> Option<u8> {
        self.network
            .tokens
            .iter()
            .find(|token| token.contract == Address::Evm(*contract))
            .map(|token| token.coin.decimals)
            .or_else(|| {
                self.decimals
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .get(contract)
                    .copied()
            })
    }

    fn symbol(&self, contract: &[u8; 20]) -> String {
        self.network
            .tokens
            .iter()
            .find(|token| token.contract == Address::Evm(*contract))
            .map_or_else(
                || "this token".to_owned(),
                |token| token.coin.symbol.clone(),
            )
    }

    async fn token_decimals(&self, contract: &[u8; 20]) -> Result<u8, ChainError> {
        if let Some(decimals) = self.known_decimals(contract) {
            return Ok(decimals);
        }
        let reported = self.call_uint(contract, &format!("0x{DECIMALS}")).await?;
        let decimals = u8::try_from(reported)
            .ok()
            .filter(|decimals| *decimals <= MAX_DECIMALS)
            .ok_or(ChainError::Invalid(
                "this token reports impossible decimals",
            ))?;
        self.decimals
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(*contract, decimals);
        Ok(decimals)
    }

    async fn call_text(&self, contract: &[u8; 20], selector: &str) -> Result<String, ChainError> {
        let reply = self
            .rpc
            .call(
                "eth_call",
                json!([{"to": hex(contract), "data": format!("0x{selector}")}, "latest"]),
            )
            .await?;
        let data = reply
            .as_str()
            .and_then(|text| text.strip_prefix("0x"))
            .and_then(|digits| alloy_primitives::hex::decode(digits).ok())
            .ok_or_else(ChainError::malformed)?;
        if data.is_empty() {
            return Err(ChainError::Invalid("this address is not a token contract"));
        }
        abi_text(&data).ok_or(ChainError::Invalid("this token reports unreadable details"))
    }

    async fn call_uint(&self, contract: &[u8; 20], data: &str) -> Result<U256, ChainError> {
        let reply = self
            .rpc
            .call(
                "eth_call",
                json!([{"to": hex(contract), "data": data}, "latest"]),
            )
            .await?;
        if reply.as_str() == Some("0x") {
            return Err(ChainError::Invalid("this address is not a token contract"));
        }
        quantity(&reply)
    }

    async fn native_balance(&self, owner: &[u8; 20]) -> Result<U256, ChainError> {
        quantity(
            &self
                .rpc
                .call("eth_getBalance", json!([hex(owner), "latest"]))
                .await?,
        )
    }

    async fn token_balance(
        &self,
        owner: &[u8; 20],
        contract: &[u8; 20],
    ) -> Result<U256, ChainError> {
        self.call_uint(contract, &format!("0x{BALANCE_OF}{}", word(owner)))
            .await
    }

    async fn fees(&self) -> Result<(U256, U256), ChainError> {
        let (history, tip) = try_join(
            self.rpc
                .call("eth_feeHistory", json!(["0x1", "latest", []])),
            self.rpc.call("eth_maxPriorityFeePerGas", json!([])),
        )
        .await?;
        let next = history["baseFeePerGas"]
            .as_array()
            .and_then(|fees| fees.last())
            .ok_or(ChainError::Unsupported("networks without EIP-1559 fees"))?;
        Ok((quantity(next)?, quantity(&tip)?))
    }
}

impl EvmChain {
    fn sendable(&self, transfer: &Transfer) -> Result<(), ChainError> {
        self.serves(&transfer.asset)?;
        if transfer.amount.is_zero() {
            return Err(ChainError::Invalid("send more than zero"));
        }
        Ok(())
    }

    fn serves(&self, asset: &AssetId) -> Result<(), ChainError> {
        if asset.network() == &self.network.id {
            Ok(())
        } else {
            Err(ChainError::Invalid("the asset belongs to another network"))
        }
    }

    async fn held(&self, from: &[u8; 20], token: Option<&[u8; 20]>) -> Result<Held, ChainError> {
        let native = self.native_balance(from);
        match token {
            None => Ok(Held {
                native: native.await?,
                token: U256::ZERO,
            }),
            Some(contract) => {
                let (native, token) = try_join(native, self.token_balance(from, contract)).await?;
                Ok(Held { native, token })
            }
        }
    }

    async fn resolve(&self, held: &Held, transfer: &Transfer) -> Result<Transfer, ChainError> {
        let mut transfer = transfer.clone();
        if !transfer.max {
            return Ok(transfer);
        }
        let all = match transfer.asset.contract().map(bytes).transpose()? {
            None => self.native(held.native)?,
            Some(contract) => amount(held.token, self.token_decimals(&contract).await?)?,
        };
        transfer.take_all(all)?;
        Ok(transfer)
    }

    fn leave_fees(
        &self,
        transfer: &mut Transfer,
        tx: &mut TxEip1559,
        max: U256,
    ) -> Result<(), ChainError> {
        let balance = tx.value;
        let Some(left) = balance.checked_sub(max).filter(|left| !left.is_zero()) else {
            return Err(insufficient(
                self.network.native.symbol.clone(),
                max.checked_add(U256::from(1u8))
                    .ok_or_else(ChainError::overflow)?,
                balance,
                self.network.native.decimals,
            )?);
        };
        tx.value = left;
        transfer.amount = self.native(left)?;
        Ok(())
    }

    fn memo_allowed(transfer: &Transfer) -> Result<(), ChainError> {
        if transfer.memo.is_some() && transfer.asset.contract().is_some() {
            return Err(ChainError::Unsupported("memos on token transfers"));
        }
        Ok(())
    }

    async fn route(&self, transfer: &Transfer) -> Result<Route, ChainError> {
        Self::memo_allowed(transfer)?;
        let to = bytes(&transfer.to)?;
        let units = transfer.amount.units();
        let route = match transfer.asset.contract().map(bytes).transpose()? {
            None => Route {
                call_to: to,
                value: units,
                data: match &transfer.memo {
                    Some(memo) => self.memo_to(&to, memo).await?,
                    None => Vec::new(),
                },
                token: None,
                decimals: self.network.native.decimals,
            },
            Some(contract) => Route {
                call_to: contract,
                value: U256::ZERO,
                data: transfer_data(&to, units),
                token: Some(contract),
                decimals: self.token_decimals(&contract).await?,
            },
        };
        if transfer.amount.decimals() != route.decimals {
            return Err(ChainError::Invalid(
                "the amount uses the wrong number of decimals",
            ));
        }
        Ok(route)
    }

    async fn memo_to(&self, to: &[u8; 20], memo: &str) -> Result<Vec<u8>, ChainError> {
        let code = self
            .rpc
            .call("eth_getCode", json!([hex(to), "latest"]))
            .await?;
        if code.as_str() != Some("0x") {
            return Err(ChainError::Invalid(
                "this address has contract code, a memo would run as a call",
            ));
        }
        Ok(memo.as_bytes().to_vec())
    }

    async fn estimate(&self, from: &[u8; 20], route: &Route) -> Result<u64, ChainError> {
        let call = json!([{
            "from": hex(from),
            "to": hex(&route.call_to),
            "value": format!("{:#x}", route.value),
            "data": hex(&route.data),
        }]);
        u64::try_from(quantity(&self.rpc.call("eth_estimateGas", call).await?)?)
            .map_err(|_| ChainError::Invalid("impossible gas estimate"))
    }

    async fn quote(&self, estimate: u64, gas_limit: u64) -> Result<Fees, ChainError> {
        let (base, tip) = self.fees().await?;
        let max_fee = base
            .checked_mul(U256::from(2u8))
            .and_then(|fee| fee.checked_add(tip))
            .ok_or_else(ChainError::overflow)?;
        self.priced((estimate, gas_limit), base, tip, max_fee)
    }

    fn priced(
        &self,
        (estimate, gas_limit): (u64, u64),
        base: U256,
        tip: U256,
        max_fee: U256,
    ) -> Result<Fees, ChainError> {
        let price = base
            .checked_add(tip)
            .ok_or_else(ChainError::overflow)?
            .min(max_fee);
        let max = U256::from(gas_limit)
            .checked_mul(max_fee)
            .ok_or_else(ChainError::overflow)?;
        let expected = U256::from(estimate)
            .checked_mul(price)
            .ok_or_else(ChainError::overflow)?;
        Ok(Fees {
            max_fee,
            tip,
            max,
            expected,
            lines: vec![(
                FeeItem::Gas {
                    units: estimate,
                    price: self.native(price)?,
                },
                expected,
            )],
        })
    }

    fn fee_quote(&self, fees: Fees) -> Result<FeeQuote, ChainError> {
        let lines = fees
            .lines
            .into_iter()
            .map(|(item, units)| {
                Ok(FeeLine {
                    item,
                    amount: self.native(units)?,
                })
            })
            .collect::<Result<Vec<_>, ChainError>>()?;
        FeeQuote::new(self.native(fees.max)?, self.native(fees.expected)?, lines)
    }

    fn funds(
        &self,
        held: &Held,
        transfer: &Transfer,
        route: &Route,
        max: U256,
    ) -> Result<(), ChainError> {
        let native = held.native;
        let native_decimals = self.network.native.decimals;
        let native_symbol = || self.network.native.symbol.clone();
        let units = transfer.amount.units();
        match route.token {
            None => {
                let needed = units.checked_add(max).ok_or_else(ChainError::overflow)?;
                if native < needed {
                    return Err(insufficient(
                        native_symbol(),
                        needed,
                        native,
                        native_decimals,
                    )?);
                }
            }
            Some(contract) => {
                if held.token < units {
                    return Err(insufficient(
                        self.symbol(&contract),
                        units,
                        held.token,
                        route.decimals,
                    )?);
                }
                if native < max {
                    return Err(insufficient(native_symbol(), max, native, native_decimals)?);
                }
            }
        }
        Ok(())
    }
}

struct Route {
    call_to: [u8; 20],
    value: U256,
    data: Vec<u8>,
    token: Option<[u8; 20]>,
    decimals: u8,
}

struct Fees {
    max_fee: U256,
    tip: U256,
    max: U256,
    expected: U256,
    lines: Vec<(FeeItem, U256)>,
}

impl Fees {
    fn add(&mut self, item: FeeItem, expected: U256, max: U256) -> Result<(), ChainError> {
        self.expected = self
            .expected
            .checked_add(expected)
            .ok_or_else(ChainError::overflow)?;
        self.max = self.max.checked_add(max).ok_or_else(ChainError::overflow)?;
        self.lines.push((item, expected));
        Ok(())
    }
}

impl Chain for EvmChain {
    type Plan = EvmPlan;

    fn network(&self) -> &Network {
        &self.network
    }

    fn derive(&self, seed: &Seed, account: u32) -> Result<Address, ChainError> {
        derive(seed, account)
    }

    async fn balances(
        &self,
        address: &Address,
        assets: &[AssetId],
    ) -> Result<Vec<Balance>, ChainError> {
        let owner = bytes(address)?;
        if assets
            .iter()
            .any(|asset| asset.network() != &self.network.id)
        {
            return Err(ChainError::Invalid("an asset belongs to another network"));
        }
        try_join_all(assets.iter().map(|asset| async {
            let amount = match asset.contract() {
                None => self.native(self.native_balance(&owner).await?)?,
                Some(contract) => {
                    let contract = bytes(contract)?;
                    let (decimals, units) = try_join(
                        self.token_decimals(&contract),
                        self.token_balance(&owner, &contract),
                    )
                    .await?;
                    amount(units, decimals)?
                }
            };
            Ok(Balance {
                asset: asset.clone(),
                amount,
                staked: None,
            })
        }))
        .await
    }

    async fn prepare(&self, transfer: &Transfer) -> Result<(EvmPlan, FeeQuote), ChainError> {
        let from = bytes(&transfer.from)?;
        self.serves(&transfer.asset)?;
        Self::memo_allowed(transfer)?;
        let token = transfer.asset.contract().map(bytes).transpose()?;
        let (held, nonce) = try_join(
            self.held(&from, token.as_ref()),
            self.rpc
                .call("eth_getTransactionCount", json!([hex(&from), "pending"])),
        )
        .await?;
        let nonce = u64::try_from(quantity(&nonce)?)
            .map_err(|_| ChainError::Invalid("impossible nonce"))?;
        let mut transfer = self.resolve(&held, transfer).await?;
        self.sendable(&transfer)?;
        let route = self.route(&transfer).await?;
        self.funds(&held, &transfer, &route, U256::ZERO)?;
        let estimate = self.estimate(&from, &route).await?;
        let gas_limit = if route.data.is_empty() && estimate == PLAIN_GAS {
            estimate
        } else {
            estimate.saturating_add(estimate / 5)
        };
        let mut fees = self.quote(estimate, gas_limit).await?;
        let mut tx = TxEip1559 {
            chain_id: self.chain_id,
            nonce,
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
            self.add_rollup_fees(&tx, estimate, &mut fees).await?;
        }
        if transfer.max && route.token.is_none() {
            self.leave_fees(&mut transfer, &mut tx, fees.max)?;
        }
        self.funds(&held, &transfer, &route, fees.max)?;
        Ok((EvmPlan { transfer, tx }, self.fee_quote(fees)?))
    }

    fn planned<'a>(&self, plan: &'a EvmPlan) -> &'a Transfer {
        &plan.transfer
    }

    async fn replace(
        &self,
        original: &Signed,
        account: u32,
        how: Replace,
    ) -> Result<(EvmPlan, FeeQuote), ChainError> {
        self.replacement(original, account, how).await
    }

    fn sign(&self, plan: &EvmPlan, seed: &Seed) -> Result<Signed, ChainError> {
        let key = key(seed, plan.transfer.account)?;
        if Address::Evm(secp256k1_address(&key.public_key())) != plan.transfer.from {
            return Err(ChainError::Invalid(
                "this seed does not own the sending account",
            ));
        }
        let (bytes, hash) = tx::sign(plan.tx.clone(), &key)?;
        Ok(Signed {
            id: TxId::new(hex(&hash))?,
            bytes,
        })
    }

    fn decode(&self, signed: &Signed) -> Result<Decoded, ChainError> {
        let parsed = tx::parse(&signed.bytes)?;
        if parsed.tx.chain_id != self.chain_id {
            return Err(ChainError::Mismatch("the network"));
        }
        let TxKind::Call(target) = parsed.tx.to else {
            return Err(ChainError::Invalid("contract creation is not a transfer"));
        };
        let target = target.into_array();
        let memo = memo(&parsed.tx.input);
        let (to, asset, amount) = if parsed.tx.input.is_empty() || memo.is_some() {
            (
                target,
                AssetId::Native(self.network.id.clone()),
                self.native(parsed.tx.value)?,
            )
        } else {
            let (to, units) = parse_transfer(&parsed.tx.input)
                .ok_or(ChainError::Invalid("not a plain token transfer"))?;
            if !parsed.tx.value.is_zero() {
                return Err(ChainError::Invalid(
                    "a token transfer must not also send coins",
                ));
            }
            let decimals = self.known_decimals(&target).ok_or(ChainError::Invalid(
                "unknown token, prepare the transfer first",
            ))?;
            (
                to,
                AssetId::Token(self.network.id.clone(), Address::Evm(target)),
                amount(units, decimals)?,
            )
        };
        Ok(Decoded {
            from: Address::Evm(parsed.from),
            to: Address::Evm(to),
            asset,
            amount,
            everything: false,
            sequence: Some(parsed.tx.nonce),
            expires_at: None,
            memo,
        })
    }

    async fn broadcast(&self, signed: &Signed) -> Result<TxId, ChainError> {
        let ours = |reply: &Value| {
            reply.as_str().map(str::to_ascii_lowercase).as_deref() == Some(signed.id.as_str())
        };
        self.rpc
            .call_all(
                "eth_sendRawTransaction",
                json!([hex(&signed.bytes)]),
                |reply| match reply {
                    Ok(reply) if ours(&reply) => Ok(()),
                    Ok(_) => Err(Failure::Retry(ChainError::Network(
                        "the node answered with another transaction id".to_owned(),
                    ))),
                    Err(Failure::Fatal(ChainError::Rejected(message)))
                        if already_known(&message) =>
                    {
                        Ok(())
                    }
                    Err(other) => Err(other),
                },
            )
            .await?;
        Ok(signed.id.clone())
    }

    async fn status(
        &self,
        id: &TxId,
        _from: &Address,
        _expires_at: Option<u64>,
    ) -> Result<TxStatus, ChainError> {
        let receipt = self
            .rpc
            .call("eth_getTransactionReceipt", json!([id.as_str()]))
            .await?;
        if receipt.is_null() {
            let known = self
                .rpc
                .call("eth_getTransactionByHash", json!([id.as_str()]))
                .await?;
            return Ok(if known.is_null() {
                TxStatus::NotFound
            } else {
                TxStatus::Pending
            });
        }
        let field = |name: &str| quantity(&receipt[name]);
        let block = u64::try_from(field("blockNumber")?).ok();
        let gas_used = field("gasUsed")?;
        let gas_fee = gas_used
            .checked_mul(field("effectiveGasPrice")?)
            .ok_or_else(ChainError::overflow)?;
        if receipt["status"] != "0x1" {
            return Ok(TxStatus::Failed("the transaction reverted".to_owned()));
        }
        let fee = if self.is_rollup() {
            self.rollup_paid(&receipt, gas_used)
                .await
                .and_then(|extra| gas_fee.checked_add(extra))
        } else {
            Some(gas_fee)
        };
        Ok(TxStatus::Confirmed {
            block,
            fee: fee.map(|fee| self.native(fee)).transpose()?,
            transaction: Some(id.clone()),
        })
    }

    async fn token(&self, contract: &Address) -> Result<Coin, ChainError> {
        let contract = bytes(contract)?;
        let (decimals, symbol, name) = try_join3(
            self.token_decimals(&contract),
            self.call_text(&contract, SYMBOL),
            self.call_text(&contract, NAME),
        )
        .await?;
        Ok(Coin {
            symbol,
            name,
            decimals,
            price: None,
            color: None,
        })
    }

    fn know(&self, contract: &Address, decimals: u8) {
        if let Address::Evm(contract) = contract {
            self.decimals
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .insert(*contract, decimals);
        }
    }
}

pub fn derive(seed: &Seed, account: u32) -> Result<Address, ChainError> {
    Ok(Address::Evm(secp256k1_address(
        &key(seed, account)?.public_key(),
    )))
}

fn key(seed: &Seed, account: u32) -> Result<k256::SecretKey, ChainError> {
    let path: DerivationPath = format!("m/44'/60'/0'/0/{account}")
        .parse()
        .map_err(|_| ChainError::Invalid("account index out of range"))?;
    Ok(bahaa_crypto::secp256k1(seed, &path)?)
}

fn insufficient(
    symbol: String,
    needed: U256,
    available: U256,
    decimals: u8,
) -> Result<ChainError, ChainError> {
    Ok(ChainError::Insufficient {
        symbol,
        needed: amount(needed, decimals)?,
        available: amount(available, decimals)?,
    })
}

fn already_known(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    ["already known", "already imported", "known transaction"]
        .iter()
        .any(|phrase| lower.contains(phrase))
}

fn memo(input: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(input).ok()?;
    (!text.is_empty() && is_plain_text(text, MAX_MEMO)).then(|| text.to_owned())
}

pub(crate) fn bytes(address: &Address) -> Result<[u8; 20], ChainError> {
    match address {
        Address::Evm(bytes) => Ok(*bytes),
        _ => Err(ChainError::Invalid("not an EVM address")),
    }
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    format!("0x{}", bahaa_types::hex::encode(bytes))
}

fn word(address: &[u8; 20]) -> String {
    format!("{:0>64}", &hex(address)[2..])
}

#[cfg(fuzzing)]
pub(crate) fn fuzz(value: &Value) {
    logs::fuzz(value);
    rollup::fuzz(value);
    let _ = already_known(value["message"].as_str().unwrap_or_default());
}

#[cfg(fuzzing)]
pub(crate) fn fuzz_input(input: &[u8]) {
    let _ = memo(input);
}

#[cfg(test)]
mod tests {
    use bahaa_config::Registry;
    use bahaa_crypto::Mnemonic;

    use super::*;

    fn chain(id: &str) -> EvmChain {
        let registry = Registry::builtin().unwrap();
        EvmChain::new(registry.get(&id.parse().unwrap()).unwrap().clone()).unwrap()
    }

    fn seed() -> Seed {
        Mnemonic::parse("test test test test test test test test test test test junk")
            .unwrap()
            .to_seed("")
    }

    #[test]
    fn derives_the_standard_accounts_on_every_evm_network() {
        for id in [
            "bsc",
            "ethereum",
            "polygon",
            "arbitrum",
            "avalanche",
            "base",
            "optimism",
        ] {
            let chain = chain(id);
            assert_eq!(
                chain.derive(&seed(), 0).unwrap().to_string(),
                "0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266"
            );
            assert_eq!(
                chain.derive(&seed(), 1).unwrap().to_string(),
                "0x70997970C51812dc3A010C7d01b50e0d17dc79C8"
            );
        }
    }

    #[test]
    fn refuses_networks_of_other_families() {
        let registry = Registry::builtin().unwrap();
        let tron = registry.get(&"tron".parse().unwrap()).unwrap().clone();
        assert!(matches!(EvmChain::new(tron), Err(ChainError::Invalid(_))));
    }

    #[test]
    fn pads_addresses_into_abi_words() {
        assert_eq!(
            word(&[0xab; 20]),
            format!("{}{}", "0".repeat(24), "ab".repeat(20))
        );
    }

    fn plan(chain: &EvmChain, to: [u8; 20], value: U256, input: Vec<u8>) -> EvmPlan {
        let from = chain.derive(&seed(), 0).unwrap();
        EvmPlan {
            transfer: Transfer {
                account: 0,
                from,
                to: Address::Evm(to),
                asset: AssetId::Native(chain.network.id.clone()),
                amount: chain.native(value).unwrap(),
                max: false,
                memo: None,
            },
            tx: TxEip1559 {
                chain_id: chain.chain_id,
                nonce: 4,
                gas_limit: 60_000,
                max_fee_per_gas: 3,
                max_priority_fee_per_gas: 1,
                to: TxKind::Call(to.into()),
                value,
                input: input.into(),
                ..TxEip1559::default()
            },
        }
    }

    #[test]
    fn decodes_native_and_token_transfers_from_the_signed_bytes() {
        let bsc = chain("bsc");
        let signed = bsc
            .sign(
                &plan(&bsc, [9; 20], U256::from(10u64.pow(17)), Vec::new()),
                &seed(),
            )
            .unwrap();
        let native = bsc.decode(&signed).unwrap();
        assert_eq!(
            (native.to, native.amount.to_string(), native.sequence),
            (Address::Evm([9; 20]), "0.1".to_owned(), Some(4))
        );
        assert_eq!(native.asset.to_string(), "bsc");
        let usdt = bytes(&bsc.network.tokens[0].contract).unwrap();
        let data = transfer_data(&[9; 20], U256::from(25u64) * U256::from(10u64.pow(17)));
        let token = bsc
            .decode(
                &bsc.sign(&plan(&bsc, usdt, U256::ZERO, data), &seed())
                    .unwrap(),
            )
            .unwrap();
        assert_eq!(
            (token.to, token.amount.to_string()),
            (Address::Evm([9; 20]), "2.5".to_owned())
        );
        assert_eq!(token.asset.contract(), Some(&Address::Evm(usdt)));
    }

    #[test]
    fn refuses_to_decode_what_is_not_a_plain_transfer() {
        let bsc = chain("bsc");
        let usdt = bytes(&bsc.network.tokens[0].contract).unwrap();
        let mut approve = transfer_data(&[9; 20], U256::from(1u8));
        approve[..4].copy_from_slice(&[0x09, 0x5e, 0xa7, 0xb3]);
        let signed = bsc
            .sign(&plan(&bsc, usdt, U256::ZERO, approve), &seed())
            .unwrap();
        assert!(matches!(bsc.decode(&signed), Err(ChainError::Invalid(_))));
        let both = transfer_data(&[9; 20], U256::from(1u8));
        let signed = bsc
            .sign(&plan(&bsc, usdt, U256::from(1u8), both), &seed())
            .unwrap();
        assert!(matches!(bsc.decode(&signed), Err(ChainError::Invalid(_))));
        let ethereum = chain("ethereum");
        let foreign = bsc
            .sign(&plan(&bsc, [9; 20], U256::from(1u8), Vec::new()), &seed())
            .unwrap();
        assert_eq!(
            ethereum.decode(&foreign).unwrap_err(),
            ChainError::Mismatch("the network")
        );
    }

    #[test]
    fn a_memo_rides_on_a_plain_coin_transfer_only() {
        let bsc = chain("bsc");
        let noted = bsc
            .sign(
                &plan(&bsc, [9; 20], U256::from(5u8), b"deposit 4417".to_vec()),
                &seed(),
            )
            .unwrap();
        let decoded = bsc.decode(&noted).unwrap();
        assert_eq!(decoded.asset, AssetId::Native(bsc.network.id.clone()));
        assert_eq!(decoded.memo.as_deref(), Some("deposit 4417"));
        for data in [vec![0xff, 0xfe], "pay\u{202e}lla".as_bytes().to_vec()] {
            let signed = bsc
                .sign(&plan(&bsc, [9; 20], U256::from(5u8), data), &seed())
                .unwrap();
            assert!(matches!(bsc.decode(&signed), Err(ChainError::Invalid(_))));
        }
        let plain = bsc
            .sign(&plan(&bsc, [9; 20], U256::from(5u8), Vec::new()), &seed())
            .unwrap();
        assert_eq!(bsc.decode(&plain).unwrap().memo, None);
    }

    #[test]
    fn only_the_owning_seed_signs() {
        let bsc = chain("bsc");
        let stranger = Mnemonic::from_entropy(&[1; 16]).unwrap().to_seed("");
        assert!(matches!(
            bsc.sign(&plan(&bsc, [9; 20], U256::from(1u8), Vec::new()), &stranger),
            Err(ChainError::Invalid(_))
        ));
    }
}
