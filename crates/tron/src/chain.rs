use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};

use bahaa_chain::{Balance, ChainError};
use bahaa_config::{ChainKind, Network};
use bahaa_crypto::{DerivationPath, Seed};
use bahaa_types::{Address, Amount, AssetId, MAX_DECIMALS};
use k256::elliptic_curve::sec1::ToEncodedPoint;
use ruint::aliases::U256;
use serde_json::{Value, json};
use sha3::{Digest, Keccak256};

use crate::http::{Http, hex, node_text};

const PREFIX: &str = "41";
const BALANCE_OF: &str = "balanceOf(address)";
const DECIMALS: &str = "decimals()";

#[derive(Debug)]
pub struct TronChain {
    network: Network,
    http: Http,
    decimals: Mutex<HashMap<[u8; 20], u8>>,
}

impl TronChain {
    pub fn new(network: Network) -> Result<Self, ChainError> {
        let ChainKind::Tron { genesis } = network.kind else {
            return Err(ChainError::Invalid("not a Tron network"));
        };
        let http = Http::new(network.endpoints.clone(), &genesis)?;
        Ok(Self {
            network,
            http,
            decimals: Mutex::new(HashMap::new()),
        })
    }

    pub fn network(&self) -> &Network {
        &self.network
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
                None => amount(
                    self.trx_balance(&owner).await?,
                    self.network.native.decimals,
                )?,
                Some(contract) => {
                    let contract = bytes(contract)?;
                    let decimals = self.token_decimals(&owner, &contract).await?;
                    let units = self
                        .call(&owner, &contract, BALANCE_OF, &word(&owner))
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

    async fn trx_balance(&self, owner: &[u8; 20]) -> Result<U256, ChainError> {
        let reply = self
            .http
            .post("wallet/getaccount", json!({"address": tron_hex(owner)}))
            .await?;
        let account = reply.as_object().ok_or_else(malformed)?;
        if account.is_empty() {
            return Ok(U256::ZERO);
        }
        let ours = account
            .get("address")
            .and_then(Value::as_str)
            .is_some_and(|text| text.eq_ignore_ascii_case(&tron_hex(owner)));
        if !ours {
            return Err(ChainError::Network(
                "an endpoint answered for another account".to_owned(),
            ));
        }
        match account.get("balance") {
            None => Ok(U256::ZERO),
            Some(balance) => balance.as_u64().map(U256::from).ok_or_else(malformed),
        }
    }

    fn known_decimals(&self, contract: &[u8; 20]) -> Option<u8> {
        self.network
            .tokens
            .iter()
            .find(|token| token.contract == Address::Tron(*contract))
            .map(|token| token.coin.decimals)
            .or_else(|| {
                self.decimals
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .get(contract)
                    .copied()
            })
    }

    async fn token_decimals(
        &self,
        owner: &[u8; 20],
        contract: &[u8; 20],
    ) -> Result<u8, ChainError> {
        if let Some(decimals) = self.known_decimals(contract) {
            return Ok(decimals);
        }
        let reported = self.call(owner, contract, DECIMALS, "").await?;
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

    async fn call(
        &self,
        owner: &[u8; 20],
        contract: &[u8; 20],
        selector: &str,
        parameter: &str,
    ) -> Result<U256, ChainError> {
        let reply = self
            .http
            .post(
                "wallet/triggerconstantcontract",
                json!({
                    "owner_address": tron_hex(owner),
                    "contract_address": tron_hex(contract),
                    "function_selector": selector,
                    "parameter": parameter,
                }),
            )
            .await?;
        constant_word(&reply)
    }
}

pub fn derive(seed: &Seed, account: u32) -> Result<Address, ChainError> {
    let path: DerivationPath = format!("m/44'/195'/0'/0/{account}")
        .parse()
        .map_err(|_| ChainError::Invalid("account index out of range"))?;
    let key = bahaa_crypto::secp256k1(seed, &path)?;
    let point = key.public_key().to_encoded_point(false);
    let hash = Keccak256::digest(&point.as_bytes()[1..]);
    let mut out = [0u8; 20];
    out.copy_from_slice(&hash[12..]);
    Ok(Address::Tron(out))
}

fn constant_word(reply: &Value) -> Result<U256, ChainError> {
    let outcome = &reply["result"];
    if outcome["code"] == "CONTRACT_VALIDATE_ERROR" {
        let message = node_text(&outcome["message"]);
        return Err(
            if message
                .to_ascii_lowercase()
                .contains("contract is not exist")
            {
                ChainError::Invalid("this address is not a token contract")
            } else {
                ChainError::Rejected(message)
            },
        );
    }
    let failed = reply["transaction"]["ret"]
        .as_array()
        .is_some_and(|results| results.iter().any(|result| result["ret"] == "FAILED"));
    if outcome["result"] != true || outcome.get("message").is_some() || failed {
        return Err(ChainError::Rejected(node_text(&outcome["message"])));
    }
    let word = reply["constant_result"]
        .as_array()
        .filter(|results| results.len() == 1)
        .and_then(|results| results[0].as_str())
        .filter(|word| word.len() == 64 && word.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .ok_or_else(malformed)?;
    U256::from_str_radix(word, 16).map_err(|_| malformed())
}

fn bytes(address: &Address) -> Result<[u8; 20], ChainError> {
    match address {
        Address::Tron(bytes) => Ok(*bytes),
        _ => Err(ChainError::Invalid("not a Tron address")),
    }
}

fn tron_hex(address: &[u8; 20]) -> String {
    format!("{PREFIX}{}", hex(address))
}

fn word(address: &[u8; 20]) -> String {
    format!("{:0>64}", hex(address))
}

fn amount(units: U256, decimals: u8) -> Result<Amount, ChainError> {
    Amount::new(units, decimals).map_err(|_| ChainError::Invalid("impossible decimals"))
}

fn malformed() -> ChainError {
    ChainError::Network("an endpoint sent a malformed reply".to_owned())
}

#[cfg(test)]
mod tests {
    use bahaa_config::Registry;
    use bahaa_crypto::Mnemonic;

    use super::*;

    const HARDHAT: &str = "test test test test test test test test test test test junk";

    #[test]
    fn derives_the_accounts_tronlink_derives() {
        let seed = Mnemonic::parse(HARDHAT).unwrap().to_seed("");
        let addresses: Vec<String> = (0..3)
            .map(|account| derive(&seed, account).unwrap().to_string())
            .collect();
        assert_eq!(
            addresses,
            [
                "TWer2Ygk5TEheHp3TPuYeqxmB6SsGZmaL6",
                "TPjjvMwjPoDC32V2dGDYTkLH4E5LAtBZ6C",
                "TB7mhtkvfhsRBRhe5FuRa4tFXSEyGDe4eA"
            ]
        );
    }

    #[test]
    fn refuses_networks_of_other_families() {
        let registry = Registry::builtin().unwrap();
        let bsc = registry.get(&"bsc".parse().unwrap()).unwrap().clone();
        assert!(matches!(TronChain::new(bsc), Err(ChainError::Invalid(_))));
    }

    #[test]
    fn addresses_are_written_the_way_the_node_reads_them() {
        assert_eq!(tron_hex(&[0xab; 20]), format!("41{}", "ab".repeat(20)));
        assert_eq!(
            word(&[0xab; 20]),
            format!("{}{}", "0".repeat(24), "ab".repeat(20))
        );
    }

    fn reply(word: &str) -> Value {
        json!({
            "result": {"result": true},
            "constant_result": [word],
            "transaction": {"ret": [{}]}
        })
    }

    #[test]
    fn reads_only_a_clean_constant_result() {
        assert_eq!(
            constant_word(&reply(&format!("{:064x}", 6))).unwrap(),
            U256::from(6u8)
        );
        let mut reverted = reply("");
        reverted["result"]["message"] = json!("524556455254206f70636f6465206578656375746564");
        reverted["transaction"]["ret"][0]["ret"] = json!("FAILED");
        assert_eq!(
            constant_word(&reverted),
            Err(ChainError::Rejected("REVERT opcode executed".to_owned()))
        );
        let mut silent_failure = reply(&format!("{:064x}", 6));
        silent_failure["transaction"]["ret"][0]["ret"] = json!("FAILED");
        assert!(matches!(
            constant_word(&silent_failure),
            Err(ChainError::Rejected(_))
        ));
        let missing = json!({"result": {"code": "CONTRACT_VALIDATE_ERROR", "message": "536d61727420636f6e7472616374206973206e6f742065786973742e"}});
        assert_eq!(
            constant_word(&missing),
            Err(ChainError::Invalid("this address is not a token contract"))
        );
        for bad in ["6", &format!("{:065x}", 6), &format!("0x{:062x}", 6)] {
            assert!(
                matches!(constant_word(&reply(bad)), Err(ChainError::Network(_))),
                "{bad}"
            );
        }
        let mut two = reply(&format!("{:064x}", 6));
        two["constant_result"] = json!([format!("{:064x}", 6), format!("{:064x}", 7)]);
        assert!(matches!(constant_word(&two), Err(ChainError::Network(_))));
    }
}
