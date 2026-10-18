use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};

use bahaa_chain::{
    Balance, Chain, ChainError, Decoded, FeeQuote, Signed, Transfer, TxId, TxStatus,
};
use bahaa_config::{ChainKind, Checkpoint, Network};
use bahaa_crypto::{DerivationPath, Seed, SeedKind};
use bahaa_types::{Address, Amount, AssetId, TonContract};
use ed25519_dalek::SigningKey;
use ruint::aliases::U256;

mod incoming;
mod send;
mod tokens;
mod track;

pub use send::TonPlan;

use crate::http::{Http, Item};
use crate::tl::BlockId;
use crate::wallet::{self, read_address};

#[derive(Debug)]
pub struct TonChain {
    network: Network,
    http: Http,
    wallet_id: i32,
    jetton_wallets: Mutex<HashMap<(Address, Address), Address>>,
    contracts: Mutex<HashMap<Address, TonContract>>,
}

impl TonChain {
    pub fn new(network: Network) -> Result<Self, ChainError> {
        let ChainKind::Ton {
            global_id,
            zero_state,
            checkpoint,
        } = network.kind
        else {
            return Err(ChainError::Invalid("not a TON network"));
        };
        let http = Http::new(network.endpoints.clone(), zero_state, block(&checkpoint))?;
        Ok(Self {
            network,
            http,
            wallet_id: wallet::wallet_id(global_id),
            jetton_wallets: Mutex::new(HashMap::new()),
            contracts: Mutex::new(HashMap::new()),
        })
    }

    pub fn derive_as(
        &self,
        seed: &Seed,
        account: u32,
        contract: TonContract,
    ) -> Result<Address, ChainError> {
        let public = key(seed, account)?.verifying_key().to_bytes();
        let address = wallet::address(contract, &public, self.subwallet(contract))?;
        self.contracts
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(address, contract);
        Ok(address)
    }

    fn subwallet(&self, contract: TonContract) -> i32 {
        match contract {
            TonContract::W5 => self.wallet_id,
            TonContract::V4R2 => wallet::V4R2_WALLET_ID,
        }
    }

    fn contract(&self, address: &Address) -> TonContract {
        self.contracts
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(address)
            .copied()
            .unwrap_or_default()
    }

    pub fn checkpoint(&self) -> Checkpoint {
        let key = self.http.trusted();
        Checkpoint {
            seqno: u32::try_from(key.seqno).unwrap_or(0),
            root_hash: key.root_hash,
            file_hash: key.file_hash,
        }
    }

    pub fn restore(&self, checkpoint: &Checkpoint) {
        self.http.trust(block(checkpoint));
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

    fn token_master(&self, owner: &Address, wallet: &Address) -> Option<Address> {
        self.jetton_wallets
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .find(|((known, _), found)| known == owner && *found == wallet)
            .map(|((_, master), _)| *master)
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

impl Chain for TonChain {
    type Plan = TonPlan;

    fn network(&self) -> &Network {
        &self.network
    }

    fn derive(&self, seed: &Seed, account: u32) -> Result<Address, ChainError> {
        self.derive_as(seed, account, TonContract::W5)
    }

    async fn balances(
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
                    self.http.account(address).await?.balance,
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

    async fn prepare(&self, transfer: &Transfer) -> Result<(TonPlan, FeeQuote), ChainError> {
        self.plan(transfer).await
    }

    fn sign(&self, plan: &TonPlan, seed: &Seed) -> Result<Signed, ChainError> {
        self.sign_plan(plan, seed)
    }

    fn decode(&self, signed: &Signed) -> Result<Decoded, ChainError> {
        self.decode_signed(signed)
    }

    async fn broadcast(&self, signed: &Signed) -> Result<TxId, ChainError> {
        self.send_signed(signed).await
    }

    async fn status(
        &self,
        id: &TxId,
        from: &Address,
        expires_at: Option<u64>,
    ) -> Result<TxStatus, ChainError> {
        self.track(id, from, expires_at).await
    }
}

fn key(seed: &Seed, account: u32) -> Result<SigningKey, ChainError> {
    if seed.kind() == SeedKind::Ton {
        if account != 0 {
            return Err(ChainError::Invalid("a TON recovery phrase has one account"));
        }
        return Ok(bahaa_crypto::ton_key(seed)?);
    }
    let path: DerivationPath = format!("m/44'/607'/{account}'")
        .parse()
        .map_err(|_| ChainError::Invalid("account index out of range"))?;
    Ok(bahaa_crypto::ed25519(seed, &path)?)
}

fn amount(units: U256, decimals: u8) -> Result<Amount, ChainError> {
    Amount::new(units, decimals).map_err(|_| ChainError::Invalid("impossible decimals"))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().fold(String::new(), |mut out, byte| {
        use std::fmt::Write;
        let _ = write!(out, "{byte:02x}");
        out
    })
}

fn malformed() -> ChainError {
    ChainError::Network("an endpoint sent a malformed reply".to_owned())
}

pub(crate) fn block(checkpoint: &Checkpoint) -> BlockId {
    BlockId {
        workchain: -1,
        shard: i64::MIN,
        seqno: i32::try_from(checkpoint.seqno).unwrap_or(i32::MAX),
        root_hash: checkpoint.root_hash,
        file_hash: checkpoint.file_hash,
    }
}

#[cfg(test)]
mod tests {
    use bahaa_chain::Chain;
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
    fn a_ton_phrase_has_one_w5_account_like_tonlib_computes() {
        use tonlib_core::wallet::mnemonic::KeyPair;
        use tonlib_core::wallet::ton_wallet::TonWallet;
        use tonlib_core::wallet::wallet_version::WalletVersion;
        let seed = bahaa_crypto::TonMnemonic::parse("dose ice enrich trigger test dove century still betray gas diet dune use other base gym mad law immense village world example praise game")
            .unwrap()
            .to_seed()
            .unwrap();
        let main = chain("ton").derive(&seed, 0).unwrap();
        let public = key(&seed, 0).unwrap().verifying_key().to_bytes().to_vec();
        let reference = TonWallet::new(
            WalletVersion::V5R1,
            KeyPair {
                public_key: public,
                secret_key: Vec::new(),
            },
        )
        .unwrap();
        assert_eq!(main.to_string(), reference.address.to_hex());
        assert!(matches!(
            chain("ton").derive(&seed, 1),
            Err(ChainError::Invalid(_))
        ));
        assert_ne!(chain("ton-testnet").derive(&seed, 0).unwrap(), main);
    }

    #[test]
    fn refuses_networks_of_other_families() {
        let registry = Registry::builtin().unwrap();
        let bsc = registry.get(&"bsc".parse().unwrap()).unwrap().clone();
        assert!(matches!(TonChain::new(bsc), Err(ChainError::Invalid(_))));
    }
}
