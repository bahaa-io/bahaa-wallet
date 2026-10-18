use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};

use bahaa_chain::{Balance, ChainError};
use bahaa_config::{ChainKind, Network};
use bahaa_crypto::{DerivationPath, Seed};
use bahaa_types::{Address, Amount, AssetId};
use ruint::aliases::U256;

use crate::http::{Http, Item};
use crate::wallet::{self, read_address};

#[derive(Debug)]
pub struct TonChain {
    network: Network,
    http: Http,
    wallet_id: i32,
    jetton_wallets: Mutex<HashMap<(Address, Address), Address>>,
}

impl TonChain {
    pub fn new(network: Network) -> Result<Self, ChainError> {
        let ChainKind::Ton {
            global_id,
            zero_state,
        } = network.kind
        else {
            return Err(ChainError::Invalid("not a TON network"));
        };
        let http = Http::new(network.endpoints.clone(), zero_state)?;
        Ok(Self {
            network,
            http,
            wallet_id: wallet::wallet_id(global_id),
            jetton_wallets: Mutex::new(HashMap::new()),
        })
    }

    pub fn network(&self) -> &Network {
        &self.network
    }

    pub fn derive(&self, seed: &Seed, account: u32) -> Result<Address, ChainError> {
        let path: DerivationPath = format!("m/44'/607'/{account}'")
            .parse()
            .map_err(|_| ChainError::Invalid("account index out of range"))?;
        let key = bahaa_crypto::ed25519(seed, &path)?;
        wallet::v5r1(&key.verifying_key().to_bytes(), self.wallet_id)
    }

    pub async fn balances(
        &self,
        address: &Address,
        assets: &[AssetId],
    ) -> Result<Vec<Balance>, ChainError> {
        if !matches!(address, Address::Ton { .. }) {
            return Err(ChainError::Invalid("not a TON address"));
        }
        let mut balances = Vec::with_capacity(assets.len());
        for asset in assets {
            if asset.network() != &self.network.id {
                return Err(ChainError::Invalid("an asset belongs to another network"));
            }
            let amount = match asset.contract() {
                None => amount(
                    self.http.balance(address).await?,
                    self.network.native.decimals,
                )?,
                Some(master) => {
                    let decimals = self
                        .network
                        .tokens
                        .iter()
                        .find(|token| token.contract == *master)
                        .map(|token| token.coin.decimals)
                        .ok_or(ChainError::Unsupported("custom TON tokens yet"))?;
                    amount(self.jetton_balance(address, master).await?, decimals)?
                }
            };
            balances.push(Balance {
                asset: asset.clone(),
                amount,
            });
        }
        Ok(balances)
    }

    async fn jetton_wallet(
        &self,
        owner: &Address,
        master: &Address,
    ) -> Result<Address, ChainError> {
        let known = self
            .jetton_wallets
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&(*owner, *master))
            .copied();
        if let Some(wallet) = known {
            return Ok(wallet);
        }
        let stack = self
            .http
            .run(master, "get_wallet_address", Some(owner))
            .await?
            .ok_or(ChainError::Invalid("this address is not a token contract"))?;
        let [Item::Cell(boc)] = stack.as_slice() else {
            return Err(malformed());
        };
        let wallet = read_address(boc).ok_or_else(malformed)?;
        self.jetton_wallets
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert((*owner, *master), wallet);
        Ok(wallet)
    }

    async fn jetton_balance(&self, owner: &Address, master: &Address) -> Result<U256, ChainError> {
        let wallet = self.jetton_wallet(owner, master).await?;
        let Some(stack) = self.http.run(&wallet, "get_wallet_data", None).await? else {
            return Ok(U256::ZERO);
        };
        let [
            Item::Num(balance),
            Item::Cell(holder),
            Item::Cell(minter),
            _,
        ] = stack.as_slice()
        else {
            return Err(malformed());
        };
        if read_address(holder) != Some(*owner) || read_address(minter) != Some(*master) {
            return Err(ChainError::Network(
                "a token wallet does not belong to this account".to_owned(),
            ));
        }
        Ok(*balance)
    }
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

    fn chain(id: &str) -> TonChain {
        let registry = Registry::builtin().unwrap();
        TonChain::new(registry.get(&id.parse().unwrap()).unwrap().clone()).unwrap()
    }

    #[test]
    fn mainnet_and_testnet_wallets_differ_for_the_same_key() {
        let seed = Mnemonic::parse("test test test test test test test test test test test junk")
            .unwrap()
            .to_seed("");
        let main = chain("ton").derive(&seed, 0).unwrap();
        let test = chain("ton-testnet").derive(&seed, 0).unwrap();
        assert_ne!(main, test);
        assert_ne!(main, chain("ton").derive(&seed, 1).unwrap());
        assert!(matches!(main, Address::Ton { workchain: 0, .. }));
    }

    #[test]
    fn refuses_networks_of_other_families() {
        let registry = Registry::builtin().unwrap();
        let bsc = registry.get(&"bsc".parse().unwrap()).unwrap().clone();
        assert!(matches!(TonChain::new(bsc), Err(ChainError::Invalid(_))));
    }
}
