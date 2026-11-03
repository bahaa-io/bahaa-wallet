use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::ops::Range;

use bahaa_chain::ChainError;
use bahaa_crypto::ExtendedPublic;
use bahaa_types::{Address, BitcoinScript};
use serde_json::{Value, json};

use crate::electrum::{Call, results};
use crate::script::{key_address, locking, scripthash};
use crate::tx::{Hash, MAX_MONEY, Outpoint, id_of};

use super::BitcoinChain;

pub(crate) const RECEIVE: usize = 0;
pub(crate) const CHANGE: usize = 1;

const GAP: u32 = 20;
const MAX_INDEX: u32 = 1_000;

#[derive(Debug, Default)]
pub(crate) struct Book {
    keychains: HashMap<Address, [Branch; 2]>,
    places: HashMap<Address, Place>,
}

#[derive(Debug)]
struct Branch {
    key: ExtendedPublic,
    known: Vec<Address>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Place {
    pub owner: Address,
    pub branch: usize,
    pub index: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Coin {
    pub outpoint: Outpoint,
    pub value: u64,
    pub confirmed: bool,
    pub address: Address,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Seen {
    pub txid: Hash,
    pub height: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Used {
    pub histories: Vec<(Address, Vec<Seen>)>,
    pub change: Address,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Funds {
    pub coins: Vec<Coin>,
    pub change: Address,
}

impl Funds {
    pub(crate) fn total(&self) -> Result<u64, ChainError> {
        self.coins
            .iter()
            .try_fold(0u64, |sum, coin| sum.checked_add(coin.value))
            .ok_or_else(ChainError::overflow)
    }
}

impl Book {
    pub(crate) fn register(
        &mut self,
        account: ExtendedPublic,
        testnet: bool,
    ) -> Result<Address, ChainError> {
        let branch = |index: usize| {
            Ok::<_, ChainError>(Branch {
                key: account.child(u32::try_from(index).map_err(|_| ChainError::overflow())?)?,
                known: Vec::new(),
            })
        };
        let receive = branch(RECEIVE)?;
        let first = key_address(&receive.key.child(0)?.key(), testnet);
        if let Entry::Vacant(slot) = self.keychains.entry(first) {
            slot.insert([receive, branch(CHANGE)?]);
            self.addresses(&first, RECEIVE, 0..1, testnet)?;
        }
        Ok(first)
    }

    pub(crate) fn forget(&mut self) {
        *self = Self::default();
    }

    pub(crate) fn owns(&self, owner: &Address) -> bool {
        self.keychains.contains_key(owner)
    }

    pub(crate) fn place(&self, address: &Address) -> Option<Place> {
        self.places.get(address).copied()
    }

    pub(crate) fn holds(&self, owner: &Address, address: &Address) -> bool {
        address == owner
            || self
                .place(address)
                .is_some_and(|place| place.owner == *owner)
    }

    pub(crate) fn addresses(
        &mut self,
        owner: &Address,
        branch: usize,
        range: Range<u32>,
        testnet: bool,
    ) -> Result<Vec<Address>, ChainError> {
        if range.end > MAX_INDEX {
            return Err(ChainError::Unsupported(
                "wallets with more than 1,000 used addresses",
            ));
        }
        let keychain = self.keychains.get_mut(owner).ok_or(ChainError::Invalid(
            "this account is not loaded, unlock again",
        ))?;
        let chain = &mut keychain[branch];
        for index in
            u32::try_from(chain.known.len()).map_err(|_| ChainError::overflow())?..range.end
        {
            let address = key_address(&chain.key.child(index)?.key(), testnet);
            chain.known.push(address);
            self.places.insert(
                address,
                Place {
                    owner: *owner,
                    branch,
                    index,
                },
            );
        }
        let start = usize::try_from(range.start).map_err(|_| ChainError::overflow())?;
        Ok(chain.known[start..]
            .iter()
            .take(range.len())
            .copied()
            .collect())
    }
}

impl BitcoinChain {
    pub(crate) async fn funds(&self, owner: &Address) -> Result<Funds, ChainError> {
        if !self.book().owns(owner) {
            return Ok(Funds {
                coins: self.unspent(&[*owner]).await?,
                change: *owner,
            });
        }
        let used = self.used(owner).await?;
        let addresses: Vec<Address> = used.histories.iter().map(|(address, _)| *address).collect();
        Ok(Funds {
            coins: self.unspent(&addresses).await?,
            change: used.change,
        })
    }

    pub(crate) async fn used(&self, owner: &Address) -> Result<Used, ChainError> {
        if !self.book().owns(owner) {
            let histories = self.histories(&[*owner]).await?;
            return Ok(Used {
                histories: vec![(*owner, histories.into_iter().flatten().collect())],
                change: *owner,
            });
        }
        let testnet = self.network.testnet;
        let mut histories = Vec::new();
        let mut next = [0u32; 2];
        let mut scanned = [0u32; 2];
        loop {
            let mut asked = Vec::new();
            {
                let mut book = self.book();
                for branch in [RECEIVE, CHANGE] {
                    let end = next[branch] + GAP;
                    if scanned[branch] < end {
                        let found = book.addresses(owner, branch, scanned[branch]..end, testnet)?;
                        asked.extend(
                            (scanned[branch]..)
                                .zip(found)
                                .map(|(index, address)| (branch, index, address)),
                        );
                        scanned[branch] = end;
                    }
                }
            }
            if asked.is_empty() {
                break;
            }
            let addresses: Vec<Address> = asked.iter().map(|(_, _, address)| *address).collect();
            let found = self.histories(&addresses).await?;
            for ((branch, index, address), seen) in asked.into_iter().zip(found) {
                if !seen.is_empty() {
                    histories.push((address, seen));
                    next[branch] = next[branch].max(index + 1);
                }
            }
        }
        let change = self
            .book()
            .addresses(owner, CHANGE, next[CHANGE]..next[CHANGE] + 1, testnet)?
            .first()
            .copied()
            .ok_or_else(ChainError::overflow)?;
        Ok(Used { histories, change })
    }

    async fn histories(&self, addresses: &[Address]) -> Result<Vec<Vec<Seen>>, ChainError> {
        let calls: Vec<Call> = addresses
            .iter()
            .map(|address| Ok(("blockchain.scripthash.get_history", json!([hash(address)?]))))
            .collect::<Result<_, ChainError>>()?;
        let lists = self.electrum.batch(None, &calls, results).await?;
        lists
            .iter()
            .map(|list| {
                list.as_array()
                    .ok_or_else(ChainError::malformed)?
                    .iter()
                    .map(|entry| seen(entry).ok_or_else(ChainError::malformed))
                    .collect()
            })
            .collect()
    }

    async fn unspent(&self, addresses: &[Address]) -> Result<Vec<Coin>, ChainError> {
        let calls: Vec<Call> = addresses
            .iter()
            .map(|address| Ok(("blockchain.scripthash.listunspent", json!([hash(address)?]))))
            .collect::<Result<_, ChainError>>()?;
        let lists = self.electrum.batch(None, &calls, results).await?;
        let mut coins = Vec::new();
        for (address, list) in addresses.iter().zip(lists) {
            for entry in list.as_array().ok_or_else(ChainError::malformed)? {
                coins.push(coin(entry, *address).ok_or_else(ChainError::malformed)?);
            }
        }
        coins.sort_by_key(|coin| coin.outpoint);
        coins.dedup_by_key(|coin| coin.outpoint);
        Ok(coins)
    }
}

fn seen(entry: &Value) -> Option<Seen> {
    Some(Seen {
        txid: id_of(entry["tx_hash"].as_str()?).ok()?,
        height: entry["height"].as_i64()?,
    })
}

fn coin(entry: &Value, address: Address) -> Option<Coin> {
    let value = entry["value"]
        .as_u64()
        .filter(|value| *value <= MAX_MONEY)?;
    Some(Coin {
        outpoint: Outpoint {
            txid: id_of(entry["tx_hash"].as_str()?).ok()?,
            vout: u32::try_from(entry["tx_pos"].as_u64()?).ok()?,
        },
        value,
        confirmed: entry["height"].as_i64()? > 0,
        address,
    })
}

pub(crate) fn hash(address: &Address) -> Result<String, ChainError> {
    Ok(scripthash(&locking(script(address)?)))
}

pub(crate) fn script(address: &Address) -> Result<BitcoinScript, ChainError> {
    match address {
        Address::Bitcoin { script, .. } => Ok(*script),
        _ => Err(ChainError::Invalid("not a Bitcoin address")),
    }
}

#[cfg(fuzzing)]
pub(crate) fn fuzz(value: &Value) {
    if let Some(list) = value.as_array() {
        for entry in list {
            let _ = seen(entry);
            let _ = coin(
                entry,
                Address::Bitcoin {
                    script: BitcoinScript::WitnessKeyHash([0; 20]),
                    testnet: false,
                },
            );
        }
    }
}
