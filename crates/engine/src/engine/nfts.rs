use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use bahaa_chain::ChainError;
use bahaa_config::Network;
use bahaa_store::{Listing, Mark, Marked};
use bahaa_types::{Address, AssetId, NetworkId, Nft, Trust};
use futures_util::future::join_all;
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tokio::time::Instant;

use super::{Notice, Worker};
use crate::chains::{self, AnyChain, NetworkNfts};
use crate::{EngineError, WalletId};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Collectible {
    pub wallet: WalletId,
    pub address: Address,
    pub nft: Nft,
    pub shown: bool,
    pub mark: Option<Mark>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NftHealth {
    pub network: NetworkId,
    pub loading: bool,
    pub problem: Option<String>,
    pub checked_at: Option<u64>,
    pub capped: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Collectibles {
    pub items: Vec<Collectible>,
    pub health: Vec<NftHealth>,
}

pub(super) struct Nfts {
    pub(super) listings: Vec<Listing>,
    pub(super) marks: Vec<Marked>,
    pub(super) listed: BTreeSet<NetworkId>,
    pub(super) health: Vec<NftHealth>,
    pub(super) next: Option<Instant>,
    task: Option<JoinHandle<()>>,
    every: Duration,
    out: watch::Sender<Collectibles>,
}

impl Nfts {
    pub(super) fn new(every: Duration) -> (Self, watch::Receiver<Collectibles>) {
        let (out, receiver) = watch::channel(Collectibles::default());
        let nfts = Self {
            listings: Vec::new(),
            marks: Vec::new(),
            listed: BTreeSet::new(),
            health: Vec::new(),
            next: None,
            task: None,
            every,
            out,
        };
        (nfts, receiver)
    }
}

fn shown(nft: &Nft, mark: Option<Mark>) -> bool {
    match mark {
        Some(mark) => mark == Mark::Shown,
        None => nft.trust == Trust::Trusted,
    }
}

impl Worker {
    pub(super) fn start_listing(&mut self) {
        if !self.unlocked() || self.nft.task.is_some() {
            return;
        }
        self.nft.next = None;
        let testnets = self.settings.show_testnets;
        let active: Vec<NetworkId> = self
            .chains
            .iter()
            .filter(|chain| chain.lists_nfts())
            .map(AnyChain::network)
            .filter(|network| testnets || !network.testnet)
            .map(|network| network.id.clone())
            .collect();
        self.nft
            .health
            .retain(|health| active.contains(&health.network));
        for network in &active {
            if !self
                .nft
                .health
                .iter()
                .any(|health| health.network == *network)
            {
                self.nft.health.push(NftHealth {
                    network: network.clone(),
                    loading: true,
                    problem: None,
                    checked_at: None,
                    capped: false,
                });
            }
        }
        self.publish_nfts();
        let chains = Arc::clone(&self.chains);
        let accounts = self.accounts.clone();
        let generation = self.generation;
        let notices = self.notices_tx.clone();
        self.nft.task = Some(tokio::spawn(async move {
            let reads = chains
                .iter()
                .filter(|chain| active.contains(&chain.network().id))
                .map(|chain| {
                    let (accounts, notices) = (&accounts, &notices);
                    async move {
                        let read = chains::nfts(chain, accounts).await;
                        let _ = notices.send(Notice::Nfts(generation, Box::new(read))).await;
                    }
                });
            join_all(reads).await;
            let _ = notices.send(Notice::Listed(generation)).await;
        }));
    }

    pub(super) fn stop_listing(&mut self) {
        if let Some(task) = self.nft.task.take() {
            task.abort();
        }
        self.nft.next = None;
    }

    pub(super) fn nfts_arrived(&mut self, update: NetworkNfts) {
        let mut problem = update.problem;
        for (address, found) in update.found {
            let listing = Listing {
                network: update.network.clone(),
                address,
                items: found.items,
                capped: found.capped,
                at: update.checked_at,
            };
            if let Err(err) = self.store.save_listing(&listing) {
                problem = Some(err.to_string());
                continue;
            }
            self.nft
                .listings
                .retain(|old| old.network != listing.network || old.address != listing.address);
            self.nft.listings.push(listing);
        }
        self.nft.listed.insert(update.network.clone());
        if let Some(health) = self
            .nft
            .health
            .iter_mut()
            .find(|health| health.network == update.network)
        {
            health.problem = problem;
            health.checked_at = Some(update.checked_at);
        }
        self.publish_nfts();
    }

    pub(super) fn listing_done(&mut self) {
        self.nft.task = None;
        self.nft.next = Some(Instant::now() + self.nft.every);
    }

    pub(super) fn mark_nft(
        &mut self,
        asset: &AssetId,
        mark: Option<Mark>,
    ) -> Result<(), EngineError> {
        self.ensure_unlocked()?;
        if !self.holds_nft(asset) {
            return Err(ChainError::Invalid("this wallet doesn't hold that NFT").into());
        }
        self.store.save_mark(asset, mark)?;
        self.nft.marks.retain(|marked| marked.asset != *asset);
        if let Some(mark) = mark {
            self.nft.marks.push(Marked {
                asset: asset.clone(),
                mark,
            });
        }
        self.publish_nfts();
        Ok(())
    }

    fn holds_nft(&self, asset: &AssetId) -> bool {
        self.held_nfts().any(|(_, _, nft)| nft.asset == *asset)
    }

    fn held_nfts(&self) -> impl Iterator<Item = (WalletId, &Address, &Nft)> {
        self.nft.listings.iter().flat_map(move |listing| {
            let family = self.registry.get(&listing.network).map(Network::family);
            let owner = self.accounts.iter().find(|account| {
                account.address == listing.address
                    && family.is_some_and(|family| account.serves(&listing.network, family))
            });
            owner.into_iter().flat_map(move |owner| {
                listing
                    .items
                    .iter()
                    .map(move |nft| (owner.wallet, &listing.address, nft))
            })
        })
    }

    pub(super) fn publish_nfts(&self) {
        let mut items: Vec<Collectible> = self
            .held_nfts()
            .map(|(wallet, address, nft)| {
                let mark = self
                    .nft
                    .marks
                    .iter()
                    .find(|marked| marked.asset == nft.asset)
                    .map(|marked| marked.mark);
                Collectible {
                    wallet,
                    address: *address,
                    nft: nft.clone(),
                    shown: shown(nft, mark),
                    mark,
                }
            })
            .collect();
        items.sort_by(|a, b| {
            let collection = |item: &Collectible| {
                item.nft
                    .collection
                    .as_ref()
                    .and_then(|collection| collection.name.clone())
            };
            (a.nft.asset.network(), collection(a), &a.nft.asset).cmp(&(
                b.nft.asset.network(),
                collection(b),
                &b.nft.asset,
            ))
        });
        let health = self
            .nft
            .health
            .iter()
            .map(|health| NftHealth {
                loading: !self.nft.listed.contains(&health.network),
                capped: self.nft.listings.iter().any(|listing| {
                    listing.network == health.network
                        && listing.capped
                        && self
                            .accounts
                            .iter()
                            .any(|account| account.address == listing.address)
                }),
                ..health.clone()
            })
            .collect();
        let collectibles = Collectibles { items, health };
        self.nft.out.send_if_modified(|current| {
            let changed = *current != collectibles;
            if changed {
                *current = collectibles;
            }
            changed
        });
    }
}

#[cfg(test)]
mod tests {
    use bahaa_types::{Amount, NftKind};

    use super::*;

    fn nft(trust: Trust) -> Nft {
        Nft {
            asset: "ton/nft/0:62f9d7e6635da5377c4b1386f5f82de306f3ac20819012ba3790a9db6e6eaa58"
                .parse()
                .unwrap(),
            kind: NftKind::TonItem,
            amount: Amount::parse("1", 0).unwrap(),
            collection: None,
            name: None,
            link: None,
            trust,
            lock: None,
        }
    }

    #[test]
    fn only_trusted_items_show_unless_the_user_decided() {
        for (trust, mark, expected) in [
            (Trust::Trusted, None, true),
            (Trust::Unknown, None, false),
            (Trust::Flagged, None, false),
            (Trust::Unknown, Some(Mark::Shown), true),
            (Trust::Flagged, Some(Mark::Shown), true),
            (Trust::Trusted, Some(Mark::Hidden), false),
        ] {
            assert_eq!(shown(&nft(trust), mark), expected, "{trust:?} {mark:?}");
        }
    }
}
