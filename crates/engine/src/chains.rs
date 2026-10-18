use std::time::{SystemTime, UNIX_EPOCH};

use bahaa_chain::{
    Balance, Chain, ChainError, Decoded, FeeQuote, Incoming, ScanCursor, Scanned, Signed, Transfer,
    TxId, TxStatus,
};
use bahaa_config::{Network, Registry};
use bahaa_crypto::Seed;
use bahaa_evm::{EvmChain, EvmPlan};
use bahaa_store::{CustomToken, Received, Scan};
use bahaa_tron::{TronChain, TronPlan};
use bahaa_types::{Address, AssetId, ChainFamily, NetworkId};
use futures_util::future::join_all;

use crate::portfolio::{Account, Held, NetworkHealth};

pub(crate) enum AnyChain {
    Evm(EvmChain),
    Tron(TronChain),
}

#[derive(Clone)]
pub(crate) enum AnyPlan {
    Evm(EvmPlan),
    Tron(TronPlan),
}

impl AnyChain {
    pub(crate) fn network(&self) -> &Network {
        match self {
            Self::Evm(chain) => chain.network(),
            Self::Tron(chain) => chain.network(),
        }
    }

    pub(crate) fn family(&self) -> ChainFamily {
        self.network().family()
    }

    pub(crate) fn derive(&self, seed: &Seed, account: u32) -> Result<Address, ChainError> {
        match self {
            Self::Evm(_) => bahaa_evm::derive(seed, account),
            Self::Tron(_) => bahaa_tron::derive(seed, account),
        }
    }

    async fn balances(
        &self,
        address: &Address,
        assets: &[AssetId],
    ) -> Result<Vec<Balance>, ChainError> {
        match self {
            Self::Evm(chain) => chain.balances(address, assets).await,
            Self::Tron(chain) => chain.balances(address, assets).await,
        }
    }

    async fn scan(
        &self,
        address: &Address,
        assets: &[AssetId],
        cursor: Option<ScanCursor>,
    ) -> Result<Option<Scanned>, ChainError> {
        match self {
            Self::Evm(chain) if chain.can_scan() => chain.scan(address, assets, cursor).await,
            Self::Tron(chain) if chain.can_scan() => chain.scan(address, assets, cursor).await,
            Self::Evm(_) | Self::Tron(_) => Ok(None),
        }
    }

    pub(crate) async fn prepare(
        &self,
        transfer: &Transfer,
    ) -> Result<(AnyPlan, FeeQuote), ChainError> {
        match self {
            Self::Evm(chain) => {
                let (plan, fee) = chain.prepare(transfer).await?;
                Ok((AnyPlan::Evm(plan), fee))
            }
            Self::Tron(chain) => {
                let (plan, fee) = chain.prepare(transfer).await?;
                Ok((AnyPlan::Tron(plan), fee))
            }
        }
    }

    pub(crate) fn sign(&self, plan: &AnyPlan, seed: &Seed) -> Result<Signed, ChainError> {
        match (self, plan) {
            (Self::Evm(chain), AnyPlan::Evm(plan)) => chain.sign(plan, seed),
            (Self::Tron(chain), AnyPlan::Tron(plan)) => chain.sign(plan, seed),
            _ => Err(ChainError::Mismatch("the network")),
        }
    }

    pub(crate) fn decode(&self, signed: &Signed) -> Result<Decoded, ChainError> {
        match self {
            Self::Evm(chain) => chain.decode(signed),
            Self::Tron(chain) => chain.decode(signed),
        }
    }

    pub(crate) async fn broadcast(&self, signed: &Signed) -> Result<TxId, ChainError> {
        match self {
            Self::Evm(chain) => chain.broadcast(signed).await,
            Self::Tron(chain) => chain.broadcast(signed).await,
        }
    }

    pub(crate) async fn status(&self, id: &TxId) -> Result<TxStatus, ChainError> {
        match self {
            Self::Evm(chain) => chain.status(id).await,
            Self::Tron(chain) => chain.status(id).await,
        }
    }
}

pub(crate) fn build(registry: &Registry) -> Vec<AnyChain> {
    registry
        .networks()
        .filter_map(|network| match network.family() {
            ChainFamily::Evm => EvmChain::new(network.clone()).ok().map(AnyChain::Evm),
            ChainFamily::Tron => TronChain::new(network.clone()).ok().map(AnyChain::Tron),
            ChainFamily::Ton => None,
        })
        .collect()
}

pub(crate) fn per_family(chains: &[AnyChain]) -> Vec<&AnyChain> {
    let mut picked: Vec<&AnyChain> = Vec::new();
    for chain in chains {
        if !picked.iter().any(|known| known.family() == chain.family()) {
            picked.push(chain);
        }
    }
    picked.sort_by_key(|chain| chain.family());
    picked
}

pub(crate) struct Fetched {
    pub generation: u64,
    pub held: Vec<Held>,
    pub health: Vec<NetworkHealth>,
    pub scans: Vec<(Scan, Vec<Received>)>,
}

struct NetworkFetch {
    held: Vec<Held>,
    health: NetworkHealth,
    scans: Vec<(Scan, Vec<Received>)>,
}

pub(crate) async fn fetch(
    chains: &[AnyChain],
    accounts: &[Account],
    custom: &[CustomToken],
    cursors: &[Scan],
    testnets: bool,
    generation: u64,
) -> Fetched {
    let jobs = chains
        .iter()
        .filter(|chain| testnets || !chain.network().testnet)
        .map(|chain| fetch_network(chain, accounts, custom, cursors));
    let results = join_all(jobs).await;
    let mut fetched = Fetched {
        generation,
        held: Vec::new(),
        health: Vec::with_capacity(results.len()),
        scans: Vec::new(),
    };
    for result in results {
        fetched.held.extend(result.held);
        fetched.health.push(result.health);
        fetched.scans.extend(result.scans);
    }
    fetched
}

async fn fetch_network(
    chain: &AnyChain,
    accounts: &[Account],
    custom: &[CustomToken],
    cursors: &[Scan],
) -> NetworkFetch {
    let network = chain.network();
    let mut assets: Vec<AssetId> = network.assets().map(|(asset, _)| asset).collect();
    for token in custom
        .iter()
        .filter(|token| token.asset.network() == &network.id)
    {
        if !assets.contains(&token.asset) {
            assets.push(token.asset.clone());
        }
    }
    let mut result = NetworkFetch {
        held: Vec::new(),
        health: NetworkHealth {
            network: network.id.clone(),
            problem: None,
            history_problem: None,
            checked_at: 0,
        },
        scans: Vec::new(),
    };
    for account in accounts
        .iter()
        .filter(|account| account.family == chain.family())
    {
        match chain.balances(&account.address, &assets).await {
            Ok(balances) => result.held.extend(balances.into_iter().map(|balance| Held {
                wallet: account.wallet,
                address: account.address,
                asset: balance.asset,
                amount: balance.amount,
            })),
            Err(err) => result.health.problem = Some(err.to_string()),
        }
        let cursor = cursors
            .iter()
            .find(|scan| scan.network == network.id && scan.address == account.address)
            .map(cursor_of);
        match chain.scan(&account.address, &assets, cursor).await {
            Ok(Some(scanned)) => result
                .scans
                .push(scan_of(&network.id, &account.address, scanned)),
            Ok(None) => {}
            Err(err) => result.health.history_problem = Some(err.to_string()),
        }
    }
    result.health.checked_at = now();
    result
}

fn cursor_of(scan: &Scan) -> ScanCursor {
    ScanCursor {
        next: scan.next,
        complete_from: scan.complete_from,
        complete_since: scan.complete_since,
    }
}

fn scan_of(network: &NetworkId, address: &Address, scanned: Scanned) -> (Scan, Vec<Received>) {
    let scan = Scan {
        network: network.clone(),
        address: *address,
        next: scanned.cursor.next,
        complete_from: scanned.cursor.complete_from,
        complete_since: scanned.cursor.complete_since,
    };
    let found = scanned
        .found
        .into_iter()
        .map(|incoming: Incoming| Received {
            id: incoming.id.to_string(),
            index: incoming.index,
            network: network.clone(),
            from: incoming.from,
            to: incoming.to,
            asset: incoming.asset,
            amount: incoming.amount,
            block: incoming.block,
            time: incoming.time,
        })
        .collect();
    (scan, found)
}

pub(crate) fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}
