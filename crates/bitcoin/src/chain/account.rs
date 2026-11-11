use std::cmp::Reverse;
use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::ops::Range;

use bahaa_chain::ChainError;
use bahaa_crypto::ExtendedPublic;
use bahaa_types::{Address, BitcoinScript, BitcoinType};

use crate::script::{key_address, locking};
use crate::source::Seen;
use crate::tx::Outpoint;

use super::BitcoinChain;

pub(crate) const RECEIVE: usize = 0;
pub(crate) const CHANGE: usize = 1;

const GAP: u32 = 20;
const MAX_INDEX: u32 = 1_000;
const KEPT_UP_TO: u64 = 10_000;

#[derive(Debug, Default)]
pub(crate) struct Book {
    keychains: HashMap<Address, Keychain>,
    places: HashMap<Address, Place>,
    coins: HashMap<Outpoint, (Address, u64)>,
}

#[derive(Debug)]
struct Keychain {
    kind: BitcoinType,
    branches: [Branch; 2],
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
    pub height: i64,
    pub address: Address,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Used {
    pub histories: Vec<(Address, Vec<Seen>)>,
    pub receive: Address,
    pub change: Address,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Funds {
    pub coins: Vec<Coin>,
    pub change: Address,
}

impl Coin {
    fn kept(&self) -> bool {
        self.value <= KEPT_UP_TO
            && matches!(
                self.address,
                Address::Bitcoin {
                    script: BitcoinScript::Taproot(_),
                    ..
                }
            )
    }
}

impl Funds {
    pub(crate) fn total(&self) -> Result<u64, ChainError> {
        sum(self.coins.iter())
    }

    pub(crate) fn available(&self) -> Result<u64, ChainError> {
        sum(self.coins.iter().filter(|coin| !coin.kept()))
    }

    pub(crate) fn spendable(&self) -> Vec<Coin> {
        let mut coins: Vec<Coin> = self
            .coins
            .iter()
            .filter(|coin| !coin.kept())
            .cloned()
            .collect();
        coins.sort_by_key(|coin| (coin.height <= 0, Reverse(coin.value)));
        coins
    }
}

fn sum<'a>(mut coins: impl Iterator<Item = &'a Coin>) -> Result<u64, ChainError> {
    coins
        .try_fold(0u64, |sum, coin| sum.checked_add(coin.value))
        .ok_or_else(ChainError::overflow)
}

impl Book {
    pub(crate) fn register(
        &mut self,
        account: ExtendedPublic,
        kind: BitcoinType,
        testnet: bool,
    ) -> Result<Address, ChainError> {
        let branch = |index: usize| {
            Ok::<_, ChainError>(Branch {
                key: account.child(u32::try_from(index).map_err(|_| ChainError::overflow())?)?,
                known: Vec::new(),
            })
        };
        let receive = branch(RECEIVE)?;
        let first = key_address(&receive.key.child(0)?.key(), kind, testnet)?;
        if let Entry::Vacant(slot) = self.keychains.entry(first) {
            slot.insert(Keychain {
                kind,
                branches: [receive, branch(CHANGE)?],
            });
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

    pub(crate) fn kind(&self, owner: &Address) -> Option<BitcoinType> {
        self.keychains.get(owner).map(|keychain| keychain.kind)
    }

    pub(crate) fn coin(&self, outpoint: &Outpoint) -> Option<(Address, u64)> {
        self.coins.get(outpoint).copied()
    }

    pub(crate) fn remember(&mut self, coins: &[Coin]) {
        self.coins.extend(
            coins
                .iter()
                .map(|coin| (coin.outpoint, (coin.address, coin.value))),
        );
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
        let kind = keychain.kind;
        let chain = &mut keychain.branches[branch];
        for index in
            u32::try_from(chain.known.len()).map_err(|_| ChainError::overflow())?..range.end
        {
            let address = key_address(&chain.key.child(index)?.key(), kind, testnet)?;
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
    pub async fn receive_address(&self, owner: &Address) -> Result<Address, ChainError> {
        if !self.book().owns(owner) {
            return Ok(*owner);
        }
        Ok(self.used(owner).await?.receive)
    }

    pub(crate) async fn funds(&self, owner: &Address) -> Result<Funds, ChainError> {
        let (addresses, change) = if self.book().owns(owner) {
            let used = self.used(owner).await?;
            let addresses: Vec<Address> =
                used.histories.iter().map(|(address, _)| *address).collect();
            (addresses, used.change)
        } else {
            (vec![*owner], *owner)
        };
        let coins = self.unspent(&addresses).await?;
        self.check(&coins).await?;
        self.book().remember(&coins);
        Ok(Funds { coins, change })
    }

    pub(crate) async fn used(&self, owner: &Address) -> Result<Used, ChainError> {
        if !self.book().owns(owner) {
            let histories = self.histories(&[*owner]).await?;
            return Ok(Used {
                histories: vec![(*owner, histories.into_iter().flatten().collect())],
                receive: *owner,
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
        let [receive, change] = [RECEIVE, CHANGE].map(|branch| {
            self.book()
                .addresses(owner, branch, next[branch]..next[branch] + 1, testnet)?
                .first()
                .copied()
                .ok_or_else(ChainError::overflow)
        });
        Ok(Used {
            histories,
            receive: receive?,
            change: change?,
        })
    }

    pub(crate) async fn histories(
        &self,
        addresses: &[Address],
    ) -> Result<Vec<Vec<Seen>>, ChainError> {
        self.sources.histories(&scripts(addresses)?).await
    }

    async fn unspent(&self, addresses: &[Address]) -> Result<Vec<Coin>, ChainError> {
        let lists = self.sources.unspent(&scripts(addresses)?).await?;
        let mut coins: Vec<Coin> = addresses
            .iter()
            .zip(lists)
            .flat_map(|(address, list)| {
                list.into_iter().map(|found| Coin {
                    outpoint: found.outpoint,
                    value: found.value,
                    height: found.height,
                    address: *address,
                })
            })
            .collect();
        coins.sort_by_key(|coin| coin.outpoint);
        coins.dedup_by_key(|coin| coin.outpoint);
        Ok(coins)
    }
}

fn scripts(addresses: &[Address]) -> Result<Vec<Vec<u8>>, ChainError> {
    addresses
        .iter()
        .map(|address| Ok(locking(script(address)?)))
        .collect()
}

pub(crate) fn script(address: &Address) -> Result<BitcoinScript, ChainError> {
    match address {
        Address::Bitcoin { script, .. } => Ok(*script),
        _ => Err(ChainError::Invalid("not a Bitcoin address")),
    }
}
