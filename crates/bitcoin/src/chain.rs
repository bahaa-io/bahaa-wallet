use std::collections::HashMap;
use std::str::FromStr;
use std::sync::{Mutex, MutexGuard, PoisonError};

use bahaa_chain::{
    Balance, Chain, ChainError, Decoded, FeeQuote, Replace, Signed, Speed, Transfer, TxId,
    TxStatus, amount, on_network,
};
use bahaa_config::{ChainKind, Coin, Network, Protocol};
use bahaa_crypto::{
    DerivationPath, SecretKey, Seed, SeedKind, compressed, secp256k1, secp256k1_extended,
};
use bahaa_types::{Address, AssetId, BitcoinType};
use ruint::aliases::U256;

mod account;
mod calls;
mod incoming;
mod proven;
mod replace;
mod send;

pub use calls::BitcoinCall;
pub use send::BitcoinPlan;

use crate::names::Resolver;
use crate::proof::{ambiguous, unproven};
use crate::script::{key_address, read};
use crate::source::Sources;
use crate::tx::{Hash, Outpoint, Output, Tx, parse};
use account::Book;
use proven::Trusted;

#[derive(Debug)]
pub struct BitcoinChain {
    network: Network,
    sources: Sources,
    names: Resolver,
    book: Mutex<Book>,
    trusted: Mutex<Trusted>,
    syncing: tokio::sync::Mutex<()>,
}

impl BitcoinChain {
    pub fn new(network: Network) -> Result<Self, ChainError> {
        let ChainKind::Bitcoin { genesis, .. } = network.kind else {
            return Err(ChainError::Invalid("not a Bitcoin network"));
        };
        Ok(Self {
            sources: Sources::new(&network, &genesis)?,
            names: Resolver::new(
                network
                    .endpoints
                    .iter()
                    .filter(|endpoint| endpoint.protocol == Protocol::DnsOverHttps)
                    .cloned()
                    .collect(),
            )?,
            network,
            book: Mutex::default(),
            trusted: Mutex::default(),
            syncing: tokio::sync::Mutex::new(()),
        })
    }

    pub async fn bip353_address(&self, name: &str) -> Result<Option<Address>, ChainError> {
        if self.network.testnet {
            return Err(ChainError::Unsupported("Bitcoin names"));
        }
        self.names.address(name, bahaa_chain::now()).await
    }

    pub fn forget(&self) {
        self.book().forget();
        self.trusted().forget();
    }

    fn book(&self) -> MutexGuard<'_, Book> {
        self.book.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn trusted(&self) -> MutexGuard<'_, Trusted> {
        self.trusted.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub fn derive_as(
        &self,
        seed: &Seed,
        account: u32,
        kind: BitcoinType,
    ) -> Result<Address, ChainError> {
        if seed.kind() == SeedKind::Bip39 {
            let key = secp256k1_extended(seed, &self.path(kind, account, "")?)?;
            return self.book().register(key, kind, self.network.testnet);
        }
        let address = self.first(seed, account, kind)?;
        self.book().single(address, kind);
        Ok(address)
    }

    pub fn first(
        &self,
        seed: &Seed,
        account: u32,
        kind: BitcoinType,
    ) -> Result<Address, ChainError> {
        let key = self.secret(seed, kind, account, "/0/0")?;
        key_address(&compressed(&key.public_key())?, kind, self.network.testnet)
    }

    fn path(
        &self,
        kind: BitcoinType,
        account: u32,
        rest: &str,
    ) -> Result<DerivationPath, ChainError> {
        let purpose = match kind {
            BitcoinType::Legacy => 44,
            BitcoinType::NestedSegwit => 49,
            BitcoinType::NativeSegwit => 84,
            BitcoinType::Taproot => 86,
            BitcoinType::LegacyUncompressed => {
                return Err(ChainError::Invalid(
                    "only single private keys have uncompressed addresses",
                ));
            }
        };
        let coin = u32::from(self.network.testnet);
        format!("m/{purpose}'/{coin}'/{account}'{rest}")
            .parse()
            .map_err(|_| ChainError::Invalid("account index out of range"))
    }

    fn secret(
        &self,
        seed: &Seed,
        kind: BitcoinType,
        account: u32,
        rest: &str,
    ) -> Result<SecretKey, ChainError> {
        let path = if seed.kind() == SeedKind::Bip39 {
            self.path(kind, account, rest)?
        } else {
            DerivationPath::from_str("m").map_err(|_| ChainError::overflow())?
        };
        Ok(secp256k1(seed, &path)?)
    }

    fn key(&self, seed: &Seed, account: u32, address: &Address) -> Result<SecretKey, ChainError> {
        let (kind, rest) = {
            let book = self.book();
            match book.place(address) {
                Some(place) => (
                    book.kind(&place.owner).unwrap_or_default(),
                    format!("/{}/{}", place.branch, place.index),
                ),
                None => (BitcoinType::default(), "/0/0".to_owned()),
            }
        };
        self.secret(seed, kind, account, &rest)
    }

    fn sats(&self, units: u64) -> Result<bahaa_types::Amount, ChainError> {
        amount(U256::from(units), self.network.native.decimals)
    }

    async fn transactions(&self, ids: &[Hash]) -> Result<Vec<Option<Tx>>, ChainError> {
        let raws = self.sources.transactions(ids).await?;
        ids.iter()
            .zip(raws)
            .map(|(id, raw)| {
                let Some(raw) = raw else {
                    return Ok(None);
                };
                let tx = parse(&raw)?;
                if ambiguous(tx.stripped_size()) {
                    return Err(unproven("a 64-byte transaction"));
                }
                if tx.id() != *id {
                    return Err(ChainError::Network(
                        "a server sent another transaction".to_owned(),
                    ));
                }
                Ok(Some(tx))
            })
            .collect()
    }

    async fn load(
        &self,
        known: &mut HashMap<Hash, Tx>,
        ids: impl IntoIterator<Item = Hash>,
    ) -> Result<(), ChainError> {
        let mut wanted: Vec<Hash> = ids
            .into_iter()
            .filter(|id| *id != [0; 32] && !known.contains_key(id))
            .collect();
        wanted.sort_unstable();
        wanted.dedup();
        for (id, tx) in wanted.iter().zip(self.transactions(&wanted).await?) {
            known.insert(*id, tx.ok_or_else(ChainError::malformed)?);
        }
        Ok(())
    }

    fn address(&self, script: &[u8]) -> Option<Address> {
        Some(Address::Bitcoin {
            script: read(script)?,
            testnet: self.network.testnet,
        })
    }

    fn native(&self, assets: &[AssetId]) -> Result<(), ChainError> {
        on_network(&self.network.id, assets)?;
        if assets.iter().any(|asset| asset.contract().is_some()) {
            return Err(ChainError::Unsupported("tokens on Bitcoin"));
        }
        Ok(())
    }
}

impl Chain for BitcoinChain {
    type Plan = BitcoinPlan;

    fn network(&self) -> &Network {
        &self.network
    }

    fn derive(&self, seed: &Seed, account: u32) -> Result<Address, ChainError> {
        self.derive_as(seed, account, BitcoinType::default())
    }

    async fn balances(
        &self,
        address: &Address,
        assets: &[AssetId],
    ) -> Result<Vec<Balance>, ChainError> {
        self.native(assets)?;
        let held = self.sats(self.funds(address).await?.total()?)?;
        Ok(assets
            .iter()
            .map(|asset| Balance {
                asset: asset.clone(),
                amount: held,
                staked: None,
            })
            .collect())
    }

    async fn prepare_with(
        &self,
        transfer: &Transfer,
        speed: Speed,
    ) -> Result<(BitcoinPlan, FeeQuote), ChainError> {
        Box::pin(self.plan(transfer, speed)).await
    }

    fn planned<'a>(&self, plan: &'a BitcoinPlan) -> &'a Transfer {
        &plan.transfer
    }

    async fn replace(
        &self,
        original: &Signed,
        account: u32,
        how: Replace,
    ) -> Result<(BitcoinPlan, FeeQuote), ChainError> {
        Box::pin(self.replacement(original, account, how)).await
    }

    fn sign(&self, plan: &BitcoinPlan, seed: &Seed) -> Result<Signed, ChainError> {
        self.signed(plan, seed)
    }

    fn decode(&self, signed: &Signed) -> Result<Decoded, ChainError> {
        self.read(signed)
    }

    async fn broadcast(&self, signed: &Signed) -> Result<TxId, ChainError> {
        self.push(signed).await
    }

    async fn status(
        &self,
        id: &TxId,
        from: &Address,
        _expires_at: Option<u64>,
    ) -> Result<TxStatus, ChainError> {
        self.lookup(id, from).await
    }

    fn token(&self, _contract: &Address) -> impl Future<Output = Result<Coin, ChainError>> + Send {
        std::future::ready(Err(ChainError::Unsupported("tokens on Bitcoin")))
    }

    fn know(&self, _contract: &Address, _decimals: u8) {}
}

fn spent<'a>(known: &'a HashMap<Hash, Tx>, point: &Outpoint) -> Option<&'a Output> {
    known
        .get(&point.txid)?
        .outputs
        .get(usize::try_from(point.vout).ok()?)
}

#[cfg(fuzzing)]
pub(crate) fn fuzz(bytes: &[u8]) {
    calls::fuzz(bytes);
}
