use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};

use bahaa_chain::{
    Balance, Chain, ChainError, Decoded, FeeQuote, Signed, Transfer, TxId, TxStatus, abi_text,
};
use bahaa_config::{ChainKind, Coin, Network};
use bahaa_crypto::{DerivationPath, Seed};
use bahaa_types::{Address, Amount, AssetId, MAX_DECIMALS};
use ruint::aliases::U256;
use serde_json::{Value, json};

mod incoming;
mod send;

pub use send::TronPlan;

use crate::http::{Http, hex, node_text};
use crate::tx::{self, Action};

const PREFIX: &str = "41";
const BALANCE_OF: &str = "balanceOf(address)";
const DECIMALS: &str = "decimals()";
const SYMBOL: &str = "symbol()";
const NAME: &str = "name()";

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

    async fn account(&self, owner: &[u8; 20]) -> Result<Option<(U256, U256)>, ChainError> {
        let reply = self
            .http
            .post("wallet/getaccount", json!({"address": tron_hex(owner)}))
            .await?;
        let account = reply.as_object().ok_or_else(malformed)?;
        if account.is_empty() {
            return Ok(None);
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
        Ok(Some((
            U256::from(count(&reply, "balance")?),
            staked(&reply)?,
        )))
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

    fn symbol(&self, contract: &[u8; 20]) -> String {
        self.network
            .tokens
            .iter()
            .find(|token| token.contract == Address::Tron(*contract))
            .map_or_else(
                || "this token".to_owned(),
                |token| token.coin.symbol.clone(),
            )
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
            .trigger(json!({
                "owner_address": tron_hex(owner),
                "contract_address": tron_hex(contract),
                "function_selector": selector,
                "parameter": parameter,
            }))
            .await?;
        constant_word(&reply)
    }

    async fn call_text(&self, contract: &[u8; 20], selector: &str) -> Result<String, ChainError> {
        let reply = self
            .trigger(json!({
                "owner_address": tron_hex(contract),
                "contract_address": tron_hex(contract),
                "function_selector": selector,
                "parameter": "",
            }))
            .await?;
        let data = reply["constant_result"]
            .as_array()
            .filter(|results| results.len() == 1)
            .and_then(|results| results[0].as_str())
            .and_then(crate::http::unhex)
            .ok_or_else(malformed)?;
        if data.is_empty() {
            return Err(ChainError::Invalid("this address is not a token contract"));
        }
        abi_text(&data).ok_or(ChainError::Invalid("this token reports unreadable details"))
    }

    async fn trigger(&self, body: Value) -> Result<Value, ChainError> {
        let reply = self
            .http
            .post("wallet/triggerconstantcontract", body)
            .await?;
        succeeded(&reply)?;
        Ok(reply)
    }
}

impl Chain for TronChain {
    type Plan = TronPlan;

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
            let (amount, staked) = match asset.contract() {
                None => {
                    let decimals = self.network.native.decimals;
                    let (free, staked) = self.account(&owner).await?.unwrap_or_default();
                    let staked = (!staked.is_zero())
                        .then(|| amount(staked, decimals))
                        .transpose()?;
                    (amount(free, decimals)?, staked)
                }
                Some(contract) => {
                    let contract = bytes(contract)?;
                    let decimals = self.token_decimals(&owner, &contract).await?;
                    let units = self
                        .call(&owner, &contract, BALANCE_OF, &word(&owner))
                        .await?;
                    (amount(units, decimals)?, None)
                }
            };
            balances.push(Balance {
                asset: asset.clone(),
                amount,
                staked,
            });
        }
        Ok(balances)
    }

    async fn prepare(&self, transfer: &Transfer) -> Result<(TronPlan, FeeQuote), ChainError> {
        self.plan(transfer).await
    }

    fn sign(&self, plan: &TronPlan, seed: &Seed) -> Result<Signed, ChainError> {
        let key = key(seed, plan.transfer.account)?;
        if Address::Tron(tx::address_of(&key.public_key())) != plan.transfer.from {
            return Err(ChainError::Invalid(
                "this seed does not own the sending account",
            ));
        }
        let (bytes, id) = tx::sign(plan.raw.clone(), &key)?;
        Ok(Signed {
            id: TxId::new(hex(&id))?,
            bytes,
        })
    }

    fn decode(&self, signed: &Signed) -> Result<Decoded, ChainError> {
        let parsed = tx::parse(&signed.bytes)?;
        if hex(&parsed.id) != signed.id.as_str() {
            return Err(ChainError::Mismatch("the transaction id"));
        }
        let (to, asset, amount) = match parsed.action {
            Action::Coins { to, sun } => (
                to,
                AssetId::Native(self.network.id.clone()),
                amount(U256::from(sun), self.network.native.decimals)?,
            ),
            Action::Token {
                contract,
                to,
                units,
            } => {
                let decimals = self.known_decimals(&contract).ok_or(ChainError::Invalid(
                    "unknown token, prepare the transfer first",
                ))?;
                (
                    to,
                    AssetId::Token(self.network.id.clone(), Address::Tron(contract)),
                    amount(units, decimals)?,
                )
            }
        };
        Ok(Decoded {
            from: Address::Tron(parsed.from),
            to: Address::Tron(to),
            asset,
            amount,
            sequence: None,
            expires_at: u64::try_from(parsed.expiration_ms).ok().map(|ms| ms / 1000),
        })
    }

    async fn broadcast(&self, signed: &Signed) -> Result<TxId, ChainError> {
        self.push(signed).await
    }

    async fn status(
        &self,
        id: &TxId,
        _from: &Address,
        _expires_at: Option<u64>,
    ) -> Result<TxStatus, ChainError> {
        self.lookup(id).await
    }

    async fn token(&self, contract: &Address) -> Result<Coin, ChainError> {
        let contract = bytes(contract)?;
        let decimals = self.token_decimals(&contract, &contract).await?;
        let symbol = self.call_text(&contract, SYMBOL).await?;
        let name = self.call_text(&contract, NAME).await?;
        Ok(Coin {
            symbol,
            name,
            decimals,
            price: None,
        })
    }

    fn know(&self, contract: &Address, decimals: u8) {
        if let Address::Tron(contract) = contract {
            self.decimals
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .insert(*contract, decimals);
        }
    }
}

pub fn derive(seed: &Seed, account: u32) -> Result<Address, ChainError> {
    Ok(Address::Tron(tx::address_of(
        &key(seed, account)?.public_key(),
    )))
}

fn key(seed: &Seed, account: u32) -> Result<k256::SecretKey, ChainError> {
    let path: DerivationPath = format!("m/44'/195'/0'/0/{account}")
        .parse()
        .map_err(|_| ChainError::Invalid("account index out of range"))?;
    Ok(bahaa_crypto::secp256k1(seed, &path)?)
}

fn succeeded(reply: &Value) -> Result<(), ChainError> {
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
    Ok(())
}

fn constant_word(reply: &Value) -> Result<U256, ChainError> {
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

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|elapsed| i64::try_from(elapsed.as_millis()).ok())
        .unwrap_or(0)
}

fn count(value: &Value, field: &str) -> Result<u64, ChainError> {
    match value.get(field) {
        None | Some(Value::Null) => Ok(0),
        Some(number) => number.as_u64().ok_or_else(malformed),
    }
}

fn staked(account: &Value) -> Result<U256, ChainError> {
    let resource = &account["account_resource"];
    let listed = |field: &str, inner: &str| -> Result<Vec<u64>, ChainError> {
        match account.get(field) {
            None | Some(Value::Null) => Ok(Vec::new()),
            Some(Value::Array(entries)) => {
                entries.iter().map(|entry| count(entry, inner)).collect()
            }
            Some(_) => Err(malformed()),
        }
    };
    let mut parts = Vec::new();
    parts.extend(listed("frozenV2", "amount")?);
    parts.extend(listed("unfrozenV2", "unfreeze_amount")?);
    parts.extend(listed("frozen", "frozen_balance")?);
    parts.push(count(account, "delegated_frozenV2_balance_for_bandwidth")?);
    parts.push(count(account, "delegated_frozen_balance_for_bandwidth")?);
    parts.push(count(resource, "delegated_frozenV2_balance_for_energy")?);
    parts.push(count(resource, "delegated_frozen_balance_for_energy")?);
    parts.push(count(
        &resource["frozen_balance_for_energy"],
        "frozen_balance",
    )?);
    parts.into_iter().try_fold(U256::ZERO, |total, part| {
        total.checked_add(U256::from(part)).ok_or_else(malformed)
    })
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
    fn staking_fields_are_summed_strictly() {
        let account = json!({
            "frozenV2": [{"amount": 5}, {"type": "ENERGY", "amount": 7}, {"type": "TRON_POWER"}],
            "unfrozenV2": [{"unfreeze_amount": 11, "unfreeze_expire_time": 1}],
            "frozen": [{"frozen_balance": 13, "expire_time": 1}],
            "delegated_frozenV2_balance_for_bandwidth": 17,
            "delegated_frozen_balance_for_bandwidth": 19,
            "acquired_delegated_frozenV2_balance_for_bandwidth": 1000,
            "account_resource": {
                "delegated_frozenV2_balance_for_energy": 23,
                "delegated_frozen_balance_for_energy": 29,
                "acquired_delegated_frozenV2_balance_for_energy": 1000,
                "frozen_balance_for_energy": {"frozen_balance": 31, "expire_time": 1}
            }
        });
        assert_eq!(staked(&account), Ok(U256::from(155u16)));
        assert_eq!(staked(&json!({"balance": 9})), Ok(U256::ZERO));
        assert!(staked(&json!({"frozenV2": {"amount": 5}})).is_err());
        assert!(staked(&json!({"frozenV2": [{"amount": "5"}]})).is_err());
        assert!(staked(&json!({"frozenV2": [{"amount": -5}]})).is_err());
    }

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

    fn read(reply: &Value) -> Result<U256, ChainError> {
        succeeded(reply).and_then(|()| constant_word(reply))
    }

    #[test]
    fn reads_only_a_clean_constant_result() {
        assert_eq!(
            read(&reply(&format!("{:064x}", 6))).unwrap(),
            U256::from(6u8)
        );
        let mut reverted = reply("");
        reverted["result"]["message"] = json!("524556455254206f70636f6465206578656375746564");
        reverted["transaction"]["ret"][0]["ret"] = json!("FAILED");
        assert_eq!(
            read(&reverted),
            Err(ChainError::Rejected("REVERT opcode executed".to_owned()))
        );
        let mut silent_failure = reply(&format!("{:064x}", 6));
        silent_failure["transaction"]["ret"][0]["ret"] = json!("FAILED");
        assert!(matches!(
            read(&silent_failure),
            Err(ChainError::Rejected(_))
        ));
        let missing = json!({"result": {"code": "CONTRACT_VALIDATE_ERROR", "message": "536d61727420636f6e7472616374206973206e6f742065786973742e"}});
        assert_eq!(
            read(&missing),
            Err(ChainError::Invalid("this address is not a token contract"))
        );
        for bad in ["6", &format!("{:065x}", 6), &format!("0x{:062x}", 6)] {
            assert!(
                matches!(read(&reply(bad)), Err(ChainError::Network(_))),
                "{bad}"
            );
        }
        let mut two = reply(&format!("{:064x}", 6));
        two["constant_result"] = json!([format!("{:064x}", 6), format!("{:064x}", 7)]);
        assert!(matches!(read(&two), Err(ChainError::Network(_))));
    }
}
