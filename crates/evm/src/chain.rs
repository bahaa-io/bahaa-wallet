use std::collections::HashMap;
use std::fmt::Write;
use std::sync::{Mutex, PoisonError};

use alloy_consensus::TxEip1559;
use alloy_primitives::TxKind;
use bahaa_chain::{
    Balance, Chain, ChainError, Decoded, FeeItem, FeeLine, FeeQuote, Signed, Transfer, TxId,
    TxStatus,
};
use bahaa_config::{ChainKind, FeeModel, Network};
use bahaa_crypto::{DerivationPath, Seed};
use bahaa_types::{Address, Amount, AssetId, MAX_DECIMALS};
use k256::elliptic_curve::sec1::ToEncodedPoint;
use ruint::aliases::U256;
use serde_json::json;
use sha3::{Digest, Keccak256};

mod logs;

pub use logs::{Incoming, ScanCursor, Scanned};

use crate::rpc::{Rpc, quantity};
use crate::tx;

const BALANCE_OF: &str = "70a08231";
const DECIMALS: &str = "313ce567";
const PLAIN_GAS: u64 = 21_000;

#[derive(Debug)]
pub struct EvmChain {
    network: Network,
    chain_id: u64,
    rpc: Rpc,
    decimals: Mutex<HashMap<[u8; 20], u8>>,
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

    pub fn chain_id(&self) -> u64 {
        self.chain_id
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
        let history = self
            .rpc
            .call("eth_feeHistory", json!(["0x1", "latest", []]))
            .await?;
        let next = history["baseFeePerGas"]
            .as_array()
            .and_then(|fees| fees.last())
            .ok_or(ChainError::Unsupported("networks without EIP-1559 fees"))?;
        let base = quantity(next)?;
        let tip = quantity(&self.rpc.call("eth_maxPriorityFeePerGas", json!([])).await?)?;
        Ok((base, tip))
    }
}

impl EvmChain {
    fn sendable(&self, transfer: &Transfer) -> Result<(), ChainError> {
        if let ChainKind::Evm {
            fee: FeeModel::OpStack,
            ..
        } = self.network.kind
        {
            return Err(ChainError::Unsupported(
                "sending yet, Base and OP Mainnet fees arrive in 0.2",
            ));
        }
        if transfer.asset.network() != &self.network.id {
            return Err(ChainError::Invalid("the asset belongs to another network"));
        }
        if transfer.amount.is_zero() {
            return Err(ChainError::Invalid("send more than zero"));
        }
        Ok(())
    }

    async fn route(&self, transfer: &Transfer) -> Result<Route, ChainError> {
        let to = bytes(&transfer.to)?;
        let units = transfer.amount.units();
        let route = match transfer.asset.contract().map(bytes).transpose()? {
            None => Route {
                call_to: to,
                value: units,
                data: Vec::new(),
                token: None,
                decimals: self.network.native.decimals,
            },
            Some(contract) => Route {
                call_to: contract,
                value: U256::ZERO,
                data: tx::transfer_data(&to, units),
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
        let price = base.checked_add(tip).ok_or_else(overflow)?;
        let max_fee = base
            .checked_mul(U256::from(2u8))
            .and_then(|fee| fee.checked_add(tip))
            .ok_or_else(overflow)?;
        let max = U256::from(gas_limit)
            .checked_mul(max_fee)
            .ok_or_else(overflow)?;
        let expected = U256::from(estimate)
            .checked_mul(price)
            .ok_or_else(overflow)?;
        let quote = FeeQuote::new(
            self.native(max)?,
            self.native(expected)?,
            vec![FeeLine {
                item: FeeItem::Gas {
                    units: estimate,
                    price: self.native(price)?,
                },
                amount: self.native(expected)?,
            }],
        )?;
        Ok(Fees {
            quote,
            max,
            max_fee,
            tip,
        })
    }

    async fn funds(
        &self,
        from: &[u8; 20],
        transfer: &Transfer,
        route: &Route,
        max: U256,
    ) -> Result<(), ChainError> {
        let native = self.native_balance(from).await?;
        let native_decimals = self.network.native.decimals;
        let native_symbol = || self.network.native.symbol.clone();
        let units = transfer.amount.units();
        match route.token {
            None => {
                let needed = units.checked_add(max).ok_or_else(overflow)?;
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
                let held = self.token_balance(from, &contract).await?;
                if held < units {
                    return Err(insufficient(
                        self.symbol(&contract),
                        units,
                        held,
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
    quote: FeeQuote,
    max: U256,
    max_fee: U256,
    tip: U256,
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
        let mut balances = Vec::with_capacity(assets.len());
        for asset in assets {
            if asset.network() != &self.network.id {
                return Err(ChainError::Invalid("an asset belongs to another network"));
            }
            let amount = match asset.contract() {
                None => self.native(self.native_balance(&owner).await?)?,
                Some(contract) => {
                    let contract = bytes(contract)?;
                    let decimals = self.token_decimals(&contract).await?;
                    amount(self.token_balance(&owner, &contract).await?, decimals)?
                }
            };
            balances.push(Balance {
                asset: asset.clone(),
                amount,
            });
        }
        Ok(balances)
    }

    async fn prepare(&self, transfer: &Transfer) -> Result<(EvmPlan, FeeQuote), ChainError> {
        self.sendable(transfer)?;
        let from = bytes(&transfer.from)?;
        let route = self.route(transfer).await?;
        let nonce = u64::try_from(quantity(
            &self
                .rpc
                .call("eth_getTransactionCount", json!([hex(&from), "pending"]))
                .await?,
        )?)
        .map_err(|_| ChainError::Invalid("impossible nonce"))?;
        self.funds(&from, transfer, &route, U256::ZERO).await?;
        let estimate = self.estimate(&from, &route).await?;
        let gas_limit = if route.data.is_empty() && estimate == PLAIN_GAS {
            estimate
        } else {
            estimate.saturating_add(estimate / 5)
        };
        let fees = self.quote(estimate, gas_limit).await?;
        self.funds(&from, transfer, &route, fees.max).await?;
        let tx = TxEip1559 {
            chain_id: self.chain_id,
            nonce,
            gas_limit,
            max_fee_per_gas: u128::try_from(fees.max_fee).map_err(|_| overflow())?,
            max_priority_fee_per_gas: u128::try_from(fees.tip).map_err(|_| overflow())?,
            to: TxKind::Call(route.call_to.into()),
            value: route.value,
            input: route.data.into(),
            ..TxEip1559::default()
        };
        Ok((
            EvmPlan {
                transfer: transfer.clone(),
                tx,
            },
            fees.quote,
        ))
    }

    fn sign(&self, plan: &EvmPlan, seed: &Seed) -> Result<Signed, ChainError> {
        let key = key(seed, plan.transfer.account)?;
        if Address::Evm(address_of(&key.public_key())) != plan.transfer.from {
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
        let (to, asset, amount) = if parsed.tx.input.is_empty() {
            (
                target,
                AssetId::Native(self.network.id.clone()),
                self.native(parsed.tx.value)?,
            )
        } else {
            let (to, units) = tx::parse_transfer(&parsed.tx.input)
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
            sequence: Some(parsed.tx.nonce),
            expires_at: None,
        })
    }

    async fn broadcast(&self, signed: &Signed) -> Result<TxId, ChainError> {
        match self
            .rpc
            .call("eth_sendRawTransaction", json!([hex(&signed.bytes)]))
            .await
        {
            Ok(reply) => {
                if reply.as_str().map(str::to_ascii_lowercase).as_deref()
                    != Some(signed.id.as_str())
                {
                    return Err(ChainError::Network(
                        "the node answered with another transaction id".to_owned(),
                    ));
                }
                Ok(signed.id.clone())
            }
            Err(ChainError::Rejected(message)) if already_known(&message) => Ok(signed.id.clone()),
            Err(err) => Err(err),
        }
    }

    async fn status(&self, id: &TxId) -> Result<TxStatus, ChainError> {
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
        let fee = field("gasUsed")?
            .checked_mul(field("effectiveGasPrice")?)
            .ok_or(ChainError::Invalid("fee overflow"))?;
        Ok(if receipt["status"] == "0x1" {
            TxStatus::Confirmed {
                block,
                fee: Some(self.native(fee)?),
            }
        } else {
            TxStatus::Failed("the transaction reverted".to_owned())
        })
    }
}

pub fn derive(seed: &Seed, account: u32) -> Result<Address, ChainError> {
    Ok(Address::Evm(address_of(&key(seed, account)?.public_key())))
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

fn overflow() -> ChainError {
    ChainError::Invalid("fee overflow")
}

pub(crate) fn address_of(key: &k256::PublicKey) -> [u8; 20] {
    let point = key.to_encoded_point(false);
    let hash = Keccak256::digest(&point.as_bytes()[1..]);
    let mut out = [0u8; 20];
    out.copy_from_slice(&hash[12..]);
    out
}

pub(crate) fn bytes(address: &Address) -> Result<[u8; 20], ChainError> {
    match address {
        Address::Evm(bytes) => Ok(*bytes),
        _ => Err(ChainError::Invalid("not an EVM address")),
    }
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().fold(String::from("0x"), |mut out, byte| {
        let _ = write!(out, "{byte:02x}");
        out
    })
}

fn word(address: &[u8; 20]) -> String {
    format!("{:0>64}", &hex(address)[2..])
}

fn amount(units: U256, decimals: u8) -> Result<Amount, ChainError> {
    Amount::new(units, decimals).map_err(|_| ChainError::Invalid("impossible decimals"))
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
        let data = tx::transfer_data(&[9; 20], U256::from(25u64) * U256::from(10u64.pow(17)));
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
        let mut approve = tx::transfer_data(&[9; 20], U256::from(1u8));
        approve[..4].copy_from_slice(&[0x09, 0x5e, 0xa7, 0xb3]);
        let signed = bsc
            .sign(&plan(&bsc, usdt, U256::ZERO, approve), &seed())
            .unwrap();
        assert!(matches!(bsc.decode(&signed), Err(ChainError::Invalid(_))));
        let both = tx::transfer_data(&[9; 20], U256::from(1u8));
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
    fn only_the_owning_seed_signs() {
        let bsc = chain("bsc");
        let stranger = Mnemonic::from_entropy(&[1; 16]).unwrap().to_seed("");
        assert!(matches!(
            bsc.sign(&plan(&bsc, [9; 20], U256::from(1u8), Vec::new()), &stranger),
            Err(ChainError::Invalid(_))
        ));
    }
}
