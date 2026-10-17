use std::time::{SystemTime, UNIX_EPOCH};

use bahaa_chain::{Balance, Chain, ChainError};
use bahaa_config::{Network, Registry};
use bahaa_evm::EvmChain;
use bahaa_store::CustomToken;
use bahaa_types::{Address, AssetId, ChainFamily};
use futures_util::future::join_all;

use crate::portfolio::{Account, Held, NetworkHealth};

pub(crate) enum AnyChain {
    Evm(EvmChain),
}

impl AnyChain {
    pub(crate) fn network(&self) -> &Network {
        match self {
            Self::Evm(chain) => chain.network(),
        }
    }

    pub(crate) fn family(&self) -> ChainFamily {
        self.network().family()
    }

    async fn balances(
        &self,
        address: &Address,
        assets: &[AssetId],
    ) -> Result<Vec<Balance>, ChainError> {
        match self {
            Self::Evm(chain) => chain.balances(address, assets).await,
        }
    }
}

pub(crate) fn build(registry: &Registry) -> Vec<AnyChain> {
    registry
        .networks()
        .filter_map(|network| match network.family() {
            ChainFamily::Evm => EvmChain::new(network.clone()).ok().map(AnyChain::Evm),
            ChainFamily::Tron | ChainFamily::Ton => None,
        })
        .collect()
}

pub(crate) struct Fetched {
    pub generation: u64,
    pub held: Vec<Held>,
    pub health: Vec<NetworkHealth>,
}

pub(crate) async fn fetch(
    chains: &[AnyChain],
    accounts: &[Account],
    custom: &[CustomToken],
    testnets: bool,
    generation: u64,
) -> Fetched {
    let jobs = chains
        .iter()
        .filter(|chain| testnets || !chain.network().testnet)
        .map(|chain| fetch_network(chain, accounts, custom));
    let results = join_all(jobs).await;
    let mut held = Vec::new();
    let mut health = Vec::with_capacity(results.len());
    for (network_held, network_health) in results {
        held.extend(network_held);
        health.push(network_health);
    }
    Fetched {
        generation,
        held,
        health,
    }
}

async fn fetch_network(
    chain: &AnyChain,
    accounts: &[Account],
    custom: &[CustomToken],
) -> (Vec<Held>, NetworkHealth) {
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
    let mut held = Vec::new();
    let mut problem = None;
    for account in accounts
        .iter()
        .filter(|account| account.family == chain.family())
    {
        match chain.balances(&account.address, &assets).await {
            Ok(balances) => held.extend(balances.into_iter().map(|balance| Held {
                wallet: account.wallet,
                address: account.address,
                asset: balance.asset,
                amount: balance.amount,
            })),
            Err(err) => problem = Some(err.to_string()),
        }
    }
    let checked_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs());
    (
        held,
        NetworkHealth {
            network: network.id.clone(),
            problem,
            checked_at,
        },
    )
}
