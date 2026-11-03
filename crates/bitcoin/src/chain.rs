use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard, PoisonError};

use bahaa_chain::{
    Balance, Chain, ChainError, Decoded, Failure, FeeQuote, Replace, Signed, Transfer, TxId,
    TxStatus, amount, on_network,
};
use bahaa_config::{ChainKind, Coin, Network};
use bahaa_crypto::{
    DerivationPath, SecretKey, Seed, SeedKind, compressed, secp256k1, secp256k1_extended,
};
use bahaa_types::{Address, AssetId, hex};
use ruint::aliases::U256;
use serde_json::json;

mod account;
mod incoming;
mod send;

pub use send::BitcoinPlan;

use crate::electrum::{Call, Electrum, text};
use crate::script::{key_address, read};
use crate::tx::{Hash, Outpoint, Output, Tx, id_text, parse};
use account::Book;

#[derive(Debug)]
pub struct BitcoinChain {
    network: Network,
    electrum: Electrum,
    book: Mutex<Book>,
}

impl BitcoinChain {
    pub fn new(network: Network) -> Result<Self, ChainError> {
        let ChainKind::Bitcoin { genesis } = network.kind else {
            return Err(ChainError::Invalid("not a Bitcoin network"));
        };
        Ok(Self {
            electrum: Electrum::new(network.endpoints.clone(), &genesis)?,
            network,
            book: Mutex::default(),
        })
    }

    pub fn forget(&self) {
        self.book().forget();
    }

    fn book(&self) -> MutexGuard<'_, Book> {
        self.book.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn path(&self, account: u32, rest: &str) -> Result<DerivationPath, ChainError> {
        let coin = u32::from(self.network.testnet);
        format!("m/84'/{coin}'/{account}'{rest}")
            .parse()
            .map_err(|_| ChainError::Invalid("account index out of range"))
    }

    fn key(&self, seed: &Seed, account: u32, address: &Address) -> Result<SecretKey, ChainError> {
        let rest = match self.book().place(address) {
            Some(place) => format!("/{}/{}", place.branch, place.index),
            None => "/0/0".to_owned(),
        };
        Ok(secp256k1(seed, &self.path(account, &rest)?)?)
    }

    fn sats(&self, units: u64) -> Result<bahaa_types::Amount, ChainError> {
        amount(U256::from(units), self.network.native.decimals)
    }

    async fn transactions(&self, ids: &[Hash]) -> Result<Vec<Option<Tx>>, ChainError> {
        let calls: Vec<Call> = ids
            .iter()
            .map(|id| ("blockchain.transaction.get", json!([id_text(id)])))
            .collect();
        let raws = self
            .electrum
            .batch(None, &calls, |answers| {
                answers
                    .into_iter()
                    .map(|answer| match answer {
                        Ok(raw) => text(&raw)
                            .map(Some)
                            .ok_or_else(|| Failure::Retry(ChainError::malformed())),
                        Err(refusal) if missing(&refusal.message) => Ok(None),
                        Err(refusal) => Err(refusal.failure()),
                    })
                    .collect::<Result<Vec<_>, _>>()
            })
            .await?;
        ids.iter()
            .zip(raws)
            .map(|(id, raw)| {
                let Some(raw) = raw else {
                    return Ok(None);
                };
                let tx = parse(&hex::decode(&raw).ok_or_else(ChainError::malformed)?)?;
                if tx.id() != *id {
                    return Err(ChainError::Network(
                        "an Electrum server sent another transaction".to_owned(),
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
        if seed.kind() == SeedKind::Bip39 {
            let key = secp256k1_extended(seed, &self.path(account, "")?)?;
            return self.book().register(key, self.network.testnet);
        }
        let key = secp256k1(seed, &self.path(account, "/0/0")?)?;
        Ok(key_address(
            &compressed(&key.public_key())?,
            self.network.testnet,
        ))
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

    async fn prepare(&self, transfer: &Transfer) -> Result<(BitcoinPlan, FeeQuote), ChainError> {
        Box::pin(self.plan(transfer)).await
    }

    fn planned<'a>(&self, plan: &'a BitcoinPlan) -> &'a Transfer {
        &plan.transfer
    }

    fn replace(
        &self,
        _original: &Signed,
        _account: u32,
        _how: Replace,
    ) -> impl Future<Output = Result<(BitcoinPlan, FeeQuote), ChainError>> + Send {
        std::future::ready(Err(ChainError::Unsupported(
            "replacing a Bitcoin transfer yet",
        )))
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
        _from: &Address,
        _expires_at: Option<u64>,
    ) -> Result<TxStatus, ChainError> {
        self.lookup(id).await
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

fn missing(message: &str) -> bool {
    let message = message.to_lowercase();
    message.contains("missing transaction") || message.contains("no such mempool")
}

#[cfg(fuzzing)]
pub(crate) fn fuzz(value: &serde_json::Value) {
    account::fuzz(value);
    incoming::fuzz(value);
    send::fuzz(value);
}
