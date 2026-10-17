use std::collections::HashMap;
use std::fmt::Write;
use std::sync::{Mutex, PoisonError};

use bahaa_chain::{Balance, ChainError};
use bahaa_config::{ChainKind, Network};
use bahaa_crypto::{DerivationPath, Seed};
use bahaa_types::{Address, Amount, AssetId, MAX_DECIMALS};
use k256::elliptic_curve::sec1::ToEncodedPoint;
use ruint::aliases::U256;
use serde_json::json;
use sha3::{Digest, Keccak256};

use crate::rpc::{Rpc, quantity};

const BALANCE_OF: &str = "70a08231";
const DECIMALS: &str = "313ce567";

#[derive(Debug)]
pub struct EvmChain {
    network: Network,
    chain_id: u64,
    pub(crate) rpc: Rpc,
    decimals: Mutex<HashMap<[u8; 20], u8>>,
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

    pub fn network(&self) -> &Network {
        &self.network
    }

    pub fn chain_id(&self) -> u64 {
        self.chain_id
    }

    pub fn derive(&self, seed: &Seed, account: u32) -> Result<Address, ChainError> {
        let path: DerivationPath = format!("m/44'/60'/0'/0/{account}")
            .parse()
            .map_err(|_| ChainError::Invalid("account index out of range"))?;
        let key = bahaa_crypto::secp256k1(seed, &path)?;
        Ok(Address::Evm(address_of(&key.public_key())))
    }

    pub async fn balances(
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
                None => {
                    let units = quantity(
                        &self
                            .rpc
                            .call("eth_getBalance", json!([hex(&owner), "latest"]))
                            .await?,
                    )?;
                    amount(units, self.network.native.decimals)?
                }
                Some(contract) => {
                    let contract = bytes(contract)?;
                    let decimals = self.token_decimals(&contract).await?;
                    let units = self
                        .call_uint(&contract, &format!("0x{BALANCE_OF}{}", word(&owner)))
                        .await?;
                    amount(units, decimals)?
                }
            };
            balances.push(Balance {
                asset: asset.clone(),
                amount,
            });
        }
        Ok(balances)
    }

    async fn token_decimals(&self, contract: &[u8; 20]) -> Result<u8, ChainError> {
        let known = self
            .network
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
            });
        if let Some(decimals) = known {
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
        let text = reply.as_str().unwrap_or_default();
        if text == "0x" {
            return Err(ChainError::Invalid("this address is not a token contract"));
        }
        quantity(&reply)
    }
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

    #[test]
    fn derives_the_standard_accounts_on_every_evm_network() {
        let seed = Mnemonic::parse("test test test test test test test test test test test junk")
            .unwrap()
            .to_seed("");
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
                chain.derive(&seed, 0).unwrap().to_string(),
                "0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266"
            );
            assert_eq!(
                chain.derive(&seed, 1).unwrap().to_string(),
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
        let address = [0xab; 20];
        assert_eq!(
            word(&address),
            format!("{}{}", "0".repeat(24), "ab".repeat(20))
        );
    }
}
