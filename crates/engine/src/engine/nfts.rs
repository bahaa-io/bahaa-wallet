use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use bahaa_chain::{ChainError, Fetcher, Gateways, read_picture};
use bahaa_config::{Network, Registry};
use bahaa_store::{Fetched, Listing, MAX_ARRIVALS, Mark, Marked, NftArrival, Pictured};
use bahaa_types::{
    Address, AssetId, Collection, Details, MAX_NFT_TEXT, NetworkId, Nft, NftId, Picture, Trust,
    to_plain_text,
};
use futures_util::StreamExt;
use futures_util::future::join_all;
use futures_util::stream;
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tokio::time::Instant;

use super::{Event, Notice, Worker};
use crate::chains::{self, AnyChain, NetworkNfts};
use crate::{EngineError, WalletId};

mod fakes;

pub use fakes::Fake;
use fakes::Known;

const FETCH_AT_ONCE: usize = 4;
const FETCH_ROUND: usize = 16;
const RETRY_AFTER: u64 = 3_600;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Collectible {
    pub wallet: WalletId,
    pub address: Address,
    pub nft: Nft,
    pub details: Option<Details>,
    pub picture: Option<Picture>,
    pub fake: Option<Fake>,
    pub shown: bool,
    pub mark: Option<Mark>,
}

impl Collectible {
    pub fn label(&self) -> Option<String> {
        self.nft
            .name
            .clone()
            .or_else(|| {
                self.details
                    .as_ref()
                    .and_then(|details| details.name.clone())
            })
            .or_else(|| {
                let collection = self.nft.collection.as_ref()?.name.clone()?;
                Some(match self.nft.asset.nft().and_then(NftId::token) {
                    Some(token) => format!("{collection} #{token}"),
                    None => collection,
                })
            })
            .map(|label| to_plain_text(&label, MAX_NFT_TEXT))
            .filter(|label| !label.is_empty())
    }
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
    pub(super) fetched: Vec<Fetched>,
    pub(super) pictured: Vec<Pictured>,
    pub(super) arrivals: Vec<NftArrival>,
    painting: BTreeSet<AssetId>,
    pub(super) listed: BTreeSet<NetworkId>,
    pub(super) health: Vec<NftHealth>,
    pub(super) next: Option<Instant>,
    task: Option<JoinHandle<()>>,
    fetching: Option<JoinHandle<()>>,
    fetcher: Option<Arc<Fetcher>>,
    every: Duration,
    out: watch::Sender<Collectibles>,
}

impl Nfts {
    pub(super) fn new(
        every: Duration,
        gateways: Option<Gateways>,
    ) -> (Self, watch::Receiver<Collectibles>) {
        let (out, receiver) = watch::channel(Collectibles::default());
        let nfts = Self {
            listings: Vec::new(),
            marks: Vec::new(),
            fetched: Vec::new(),
            pictured: Vec::new(),
            arrivals: Vec::new(),
            painting: BTreeSet::new(),
            listed: BTreeSet::new(),
            health: Vec::new(),
            next: None,
            task: None,
            fetching: None,
            fetcher: gateways
                .and_then(|gateways| Fetcher::new(gateways).ok())
                .map(Arc::new),
            every,
            out,
        };
        (nfts, receiver)
    }

    pub(super) fn forget(&mut self) {
        if let Some(task) = self.fetching.take() {
            task.abort();
        }
        self.listings.clear();
        self.marks.clear();
        self.fetched.clear();
        self.pictured.clear();
        self.arrivals.clear();
        self.painting.clear();
    }
}

fn shown(trust: Trust, mark: Option<Mark>) -> bool {
    match mark {
        Some(mark) => mark == Mark::Shown,
        None => trust == Trust::Trusted,
    }
}

fn judged(
    nft: &Nft,
    registry: &Registry,
    known: &Known<'_>,
    details: Option<&Details>,
) -> (Nft, Option<Fake>) {
    let listed = nft
        .collection
        .as_ref()
        .and_then(|collection| collection.address.as_ref())
        .and_then(|address| registry.get(nft.asset.network())?.collection(address));
    let mut judged = nft.clone();
    if let Some(listed) = listed {
        judged.collection = Some(Collection {
            address: Some(listed.address),
            name: Some(listed.name.clone()),
        });
        if nft.trust != Trust::Flagged {
            judged.trust = Trust::Trusted;
        }
        return (judged, None);
    }
    if nft.trust != Trust::Unknown {
        return (judged, None);
    }
    let texts = [
        nft.collection
            .as_ref()
            .and_then(|collection| collection.name.as_deref()),
        nft.name.as_deref(),
        details.and_then(|details| details.name.as_deref()),
    ];
    let fake = known.fake(texts.into_iter().flatten());
    if fake.is_some() {
        judged.trust = Trust::Flagged;
    }
    (judged, fake)
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
        let before: Vec<(Address, bool, BTreeSet<AssetId>)> = self
            .nft
            .listings
            .iter()
            .filter(|old| old.network == update.network)
            .map(|old| {
                let held = old.items.iter().map(|item| item.asset.clone()).collect();
                (old.address, old.capped, held)
            })
            .collect();
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
        self.note_arrivals(&update.network, &before);
        self.publish_nfts();
        self.fetch_details();
    }

    fn note_arrivals(
        &mut self,
        network: &NetworkId,
        before: &[(Address, bool, BTreeSet<AssetId>)],
    ) {
        let now = bahaa_chain::now();
        let fresh: Vec<NftArrival> = self
            .collectibles()
            .into_iter()
            .filter(|item| item.nft.asset.network() == network)
            .filter(|item| {
                before.iter().any(|(address, capped, held)| {
                    *address == item.address && !capped && !held.contains(&item.nft.asset)
                })
            })
            .map(|item| NftArrival {
                network: network.clone(),
                address: item.address,
                name: item.label(),
                collection: item
                    .nft
                    .collection
                    .as_ref()
                    .and_then(|collection| collection.name.clone()),
                asset: item.nft.asset,
                time: now,
                shown: item.shown,
            })
            .collect();
        let saved: Vec<NftArrival> = fresh
            .into_iter()
            .filter(|arrival| self.store.save_arrival(arrival).is_ok())
            .collect();
        if saved.is_empty() {
            return;
        }
        self.nft.arrivals.extend(saved.iter().cloned());
        self.nft
            .arrivals
            .sort_by_key(|arrival| std::cmp::Reverse(arrival.time));
        let gone = self
            .nft
            .arrivals
            .split_off(self.nft.arrivals.len().min(MAX_ARRIVALS));
        for arrival in &gone {
            let _ = self.store.forget_arrival(arrival);
        }
        let _ = self.events.send(Event::NftArrived(saved));
    }

    pub(super) fn nft_label(&self, asset: &AssetId) -> Option<String> {
        self.collectibles()
            .into_iter()
            .find(|item| item.nft.asset == *asset)
            .and_then(|item| item.label())
    }

    pub(super) fn listing_done(&mut self) {
        self.nft.task = None;
        self.nft.next = Some(Instant::now() + self.nft.every);
        self.forget_unheld_details();
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
        self.fetch_details();
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

    fn collectibles(&self) -> Vec<Collectible> {
        let known = Known::new(&self.registry);
        let mut items: Vec<Collectible> =
            self.held_nfts()
                .map(|(wallet, address, nft)| {
                    let mark = self
                        .nft
                        .marks
                        .iter()
                        .find(|marked| marked.asset == nft.asset)
                        .map(|marked| marked.mark);
                    let details = self
                        .nft
                        .fetched
                        .iter()
                        .find(|fetched| {
                            fetched.asset == nft.asset && nft.link.as_ref() == Some(&fetched.link)
                        })
                        .and_then(|fetched| fetched.details.clone());
                    let picture = details
                        .as_ref()
                        .and_then(|details| details.image.as_ref())
                        .and_then(|link| {
                            self.nft.pictured.iter().find(|pictured| {
                                pictured.asset == nft.asset && pictured.link == *link
                            })
                        })
                        .and_then(|pictured| pictured.picture.clone());
                    let (nft, fake) = judged(nft, &self.registry, &known, details.as_ref());
                    Collectible {
                        wallet,
                        address: *address,
                        shown: shown(nft.trust, mark),
                        nft,
                        details,
                        picture,
                        fake,
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
        items
    }

    pub(super) fn fetch_details(&mut self) {
        if !self.unlocked() || self.nft.fetching.is_some() {
            return;
        }
        let Some(reader) = self.nft.fetcher.clone() else {
            return;
        };
        let now = bahaa_chain::now();
        let mut wanted: Vec<(AssetId, String)> = Vec::new();
        for item in self.collectibles() {
            let Some(link) = item.nft.link.filter(|_| item.shown) else {
                continue;
            };
            let fetched = self
                .nft
                .fetched
                .iter()
                .find(|fetched| fetched.asset == item.nft.asset && fetched.link == link);
            let due = fetched.is_none_or(|fetched| {
                fetched.details.is_none() && now >= fetched.at.saturating_add(RETRY_AFTER)
            });
            if due && !wanted.iter().any(|(asset, _)| *asset == item.nft.asset) {
                wanted.push((item.nft.asset, link));
            }
            if wanted.len() == FETCH_ROUND {
                break;
            }
        }
        if wanted.is_empty() {
            return;
        }
        let session = self.session;
        let notices = self.notices_tx.clone();
        self.nft.fetching = Some(tokio::spawn(async move {
            let mut reads = Vec::with_capacity(wanted.len());
            for (asset, link) in wanted {
                let reader = Arc::clone(&reader);
                reads.push(async move {
                    let details = reader.details(&link).await.ok();
                    Fetched {
                        asset,
                        link,
                        details,
                        at: bahaa_chain::now(),
                    }
                });
            }
            let fetched: Vec<Fetched> = stream::iter(reads)
                .buffer_unordered(FETCH_AT_ONCE)
                .collect()
                .await;
            let _ = notices.send(Notice::Details(session, fetched)).await;
        }));
    }

    pub(super) fn fetch_picture(&mut self, asset: AssetId) {
        if !self.unlocked() || self.nft.painting.contains(&asset) {
            return;
        }
        let Some(reader) = self.nft.fetcher.clone() else {
            return;
        };
        let wanted = self
            .collectibles()
            .into_iter()
            .find(|item| item.nft.asset == asset);
        let Some(link) = wanted
            .filter(|item| item.shown && item.picture.is_none())
            .and_then(|item| item.details)
            .and_then(|details| details.image)
        else {
            return;
        };
        let now = bahaa_chain::now();
        let failed_lately = self.nft.pictured.iter().any(|pictured| {
            pictured.asset == asset
                && pictured.link == link
                && now < pictured.at.saturating_add(RETRY_AFTER)
        });
        if failed_lately {
            return;
        }
        self.nft.painting.insert(asset.clone());
        let session = self.session;
        let notices = self.notices_tx.clone();
        tokio::spawn(async move {
            let picture = match reader.picture(&link).await {
                Ok(bytes) => tokio::task::spawn_blocking(move || read_picture(&bytes))
                    .await
                    .ok()
                    .flatten(),
                Err(_) => None,
            };
            let pictured = Pictured {
                asset,
                link,
                picture,
                at: bahaa_chain::now(),
            };
            let _ = notices
                .send(Notice::Pictures(session, vec![pictured]))
                .await;
        });
    }

    pub(super) fn pictures_arrived(&mut self, pictured: Vec<Pictured>) {
        for item in pictured {
            self.nft.painting.remove(&item.asset);
            if self.store.save_pictured(&item).is_err() {
                continue;
            }
            self.nft.pictured.retain(|old| old.asset != item.asset);
            self.nft.pictured.push(item);
        }
        self.publish_nfts();
    }

    pub(super) fn details_arrived(&mut self, fetched: Vec<Fetched>) {
        self.nft.fetching = None;
        for item in fetched {
            if self.store.save_fetched(&item).is_err() {
                continue;
            }
            self.nft.fetched.retain(|old| old.asset != item.asset);
            self.nft.fetched.push(item);
        }
        self.publish_nfts();
        self.fetch_details();
    }

    fn forget_unheld_details(&mut self) {
        let held: BTreeSet<AssetId> = self
            .held_nfts()
            .map(|(_, _, nft)| nft.asset.clone())
            .collect();
        let gone: Vec<AssetId> = self
            .nft
            .fetched
            .iter()
            .filter(|fetched| !held.contains(&fetched.asset))
            .map(|fetched| fetched.asset.clone())
            .collect();
        for asset in &gone {
            if self.store.forget_fetched(asset).is_ok() {
                self.nft.fetched.retain(|fetched| fetched.asset != *asset);
            }
        }
        let unpainted: Vec<AssetId> = self
            .nft
            .pictured
            .iter()
            .filter(|pictured| !held.contains(&pictured.asset))
            .map(|pictured| pictured.asset.clone())
            .collect();
        for asset in &unpainted {
            if self.store.forget_pictured(asset).is_ok() {
                self.nft
                    .pictured
                    .retain(|pictured| pictured.asset != *asset);
            }
        }
    }

    pub(super) fn publish_nfts(&self) {
        let items = self.collectibles();
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
            assert_eq!(
                shown(nft(trust).trust, mark),
                expected,
                "{trust:?} {mark:?}"
            );
        }
    }
}
