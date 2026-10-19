use std::time::{SystemTime, UNIX_EPOCH};

use bahaa_chain::{
    Balance, Chain, ChainError, Decoded, FeeQuote, Incoming, Replace, ScanCursor, Scanned, Signed,
    Transfer, TxId, TxStatus,
};
use bahaa_config::{Coin, Network, Registry};
use bahaa_crypto::{Seed, SeedKind};
use bahaa_evm::{EvmChain, EvmPlan};
use bahaa_store::{CustomToken, Outgoing, Received, Scan, TrustedBlock};
use bahaa_ton::{TonChain, TonPlan};
use bahaa_tron::{TronChain, TronPlan};
use bahaa_types::{Address, AssetId, ChainFamily, NetworkId, TonContract};
use futures_util::future::join_all;

use crate::WalletId;
use crate::portfolio::{Account, Held, NetworkHealth};

pub(crate) enum AnyChain {
    Evm(EvmChain),
    Tron(TronChain),
    Ton(Box<TonChain>),
}

#[derive(Clone)]
pub(crate) enum AnyPlan {
    Evm(EvmPlan),
    Tron(TronPlan),
    Ton(TonPlan),
}

impl AnyChain {
    pub(crate) fn network(&self) -> &Network {
        match self {
            Self::Evm(chain) => chain.network(),
            Self::Tron(chain) => chain.network(),
            Self::Ton(chain) => chain.network(),
        }
    }

    pub(crate) fn family(&self) -> ChainFamily {
        self.network().family()
    }

    pub(crate) fn holds(&self, kind: SeedKind) -> bool {
        matches!(self, Self::Ton(_)) || kind == SeedKind::Bip39
    }

    pub(crate) fn derive(
        &self,
        seed: &Seed,
        account: u32,
        contract: TonContract,
    ) -> Result<Address, ChainError> {
        match self {
            Self::Evm(_) => bahaa_evm::derive(seed, account),
            Self::Tron(_) => bahaa_tron::derive(seed, account),
            Self::Ton(chain) => chain.derive_as(seed, account, contract),
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
            Self::Ton(chain) => chain.balances(address, assets).await,
        }
    }

    pub(crate) async fn token(&self, contract: &Address) -> Result<Coin, ChainError> {
        match self {
            Self::Evm(chain) => chain.token(contract).await,
            Self::Tron(chain) => chain.token(contract).await,
            Self::Ton(chain) => chain.token(contract).await,
        }
    }

    pub(crate) fn know(&self, contract: &Address, decimals: u8) {
        match self {
            Self::Evm(chain) => chain.know(contract, decimals),
            Self::Tron(chain) => chain.know(contract, decimals),
            Self::Ton(chain) => chain.know(contract, decimals),
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
            Self::Ton(chain) if chain.can_scan() => chain.scan(address, assets, cursor).await,
            Self::Evm(_) | Self::Tron(_) | Self::Ton(_) => Ok(None),
        }
    }

    pub(crate) async fn prepare(
        &self,
        transfer: &Transfer,
    ) -> Result<(AnyPlan, Transfer, FeeQuote), ChainError> {
        match self {
            Self::Evm(chain) => {
                let (plan, fee) = chain.prepare(transfer).await?;
                let planned = chain.planned(&plan).clone();
                Ok((AnyPlan::Evm(plan), planned, fee))
            }
            Self::Tron(chain) => {
                let (plan, fee) = chain.prepare(transfer).await?;
                let planned = chain.planned(&plan).clone();
                Ok((AnyPlan::Tron(plan), planned, fee))
            }
            Self::Ton(chain) => {
                let (plan, fee) = chain.prepare(transfer).await?;
                let planned = chain.planned(&plan).clone();
                Ok((AnyPlan::Ton(plan), planned, fee))
            }
        }
    }

    pub(crate) async fn replace(
        &self,
        original: &Signed,
        account: u32,
        how: Replace,
    ) -> Result<(AnyPlan, Transfer, FeeQuote), ChainError> {
        match self {
            Self::Evm(chain) => {
                let (plan, fee) = chain.replace(original, account, how).await?;
                let planned = chain.planned(&plan).clone();
                Ok((AnyPlan::Evm(plan), planned, fee))
            }
            Self::Tron(chain) => {
                let (plan, fee) = chain.replace(original, account, how).await?;
                let planned = chain.planned(&plan).clone();
                Ok((AnyPlan::Tron(plan), planned, fee))
            }
            Self::Ton(chain) => {
                let (plan, fee) = chain.replace(original, account, how).await?;
                let planned = chain.planned(&plan).clone();
                Ok((AnyPlan::Ton(plan), planned, fee))
            }
        }
    }

    pub(crate) fn sign(&self, plan: &AnyPlan, seed: &Seed) -> Result<Signed, ChainError> {
        match (self, plan) {
            (Self::Evm(chain), AnyPlan::Evm(plan)) => chain.sign(plan, seed),
            (Self::Tron(chain), AnyPlan::Tron(plan)) => chain.sign(plan, seed),
            (Self::Ton(chain), AnyPlan::Ton(plan)) => chain.sign(plan, seed),
            _ => Err(ChainError::Mismatch("the network")),
        }
    }

    pub(crate) fn decode(&self, signed: &Signed) -> Result<Decoded, ChainError> {
        match self {
            Self::Evm(chain) => chain.decode(signed),
            Self::Tron(chain) => chain.decode(signed),
            Self::Ton(chain) => chain.decode(signed),
        }
    }

    pub(crate) async fn broadcast(&self, signed: &Signed) -> Result<TxId, ChainError> {
        match self {
            Self::Evm(chain) => chain.broadcast(signed).await,
            Self::Tron(chain) => chain.broadcast(signed).await,
            Self::Ton(chain) => chain.broadcast(signed).await,
        }
    }

    pub(crate) async fn status(
        &self,
        id: &TxId,
        from: &Address,
        expires_at: Option<u64>,
    ) -> Result<TxStatus, ChainError> {
        match self {
            Self::Evm(chain) => chain.status(id, from, expires_at).await,
            Self::Tron(chain) => chain.status(id, from, expires_at).await,
            Self::Ton(chain) => chain.status(id, from, expires_at).await,
        }
    }
}

pub(crate) fn build(registry: &Registry) -> Vec<AnyChain> {
    registry
        .networks()
        .filter_map(|network| match network.family() {
            ChainFamily::Evm => EvmChain::new(network.clone()).ok().map(AnyChain::Evm),
            ChainFamily::Tron => TronChain::new(network.clone()).ok().map(AnyChain::Tron),
            ChainFamily::Ton => TonChain::new(network.clone())
                .ok()
                .map(|chain| AnyChain::Ton(Box::new(chain))),
        })
        .collect()
}

pub(crate) fn ton_mainnet(chains: &[AnyChain]) -> Option<&TonChain> {
    chains.iter().find_map(|chain| match chain {
        AnyChain::Ton(ton) if !ton.network().testnet => Some(&**ton),
        _ => None,
    })
}

pub(crate) fn know(chains: &[AnyChain], custom: &[CustomToken]) {
    for token in custom {
        if let Some(chain) = chains
            .iter()
            .find(|chain| &chain.network().id == token.asset.network())
            && let Some(contract) = token.asset.contract()
        {
            chain.know(contract, token.coin.decimals);
        }
    }
}

pub(crate) fn restore(chains: &[AnyChain], saved: &[TrustedBlock]) {
    for chain in chains {
        if let AnyChain::Ton(ton) = chain
            && let Some(block) = saved.iter().find(|block| block.network == ton.network().id)
        {
            ton.restore(&block.checkpoint);
        }
    }
}

pub(crate) fn trusted(chains: &[AnyChain]) -> Vec<TrustedBlock> {
    chains
        .iter()
        .filter_map(|chain| match chain {
            AnyChain::Ton(ton) => Some(TrustedBlock {
                network: ton.network().id.clone(),
                checkpoint: ton.checkpoint(),
            }),
            AnyChain::Evm(_) | AnyChain::Tron(_) => None,
        })
        .collect()
}

pub(crate) fn accounts(
    wallet: WalletId,
    index: u32,
    derived: &[(ChainFamily, NetworkId, Address)],
) -> Vec<Account> {
    let mut families: Vec<ChainFamily> = derived.iter().map(|(family, ..)| *family).collect();
    families.sort();
    families.dedup();
    let mut accounts = Vec::new();
    for family in families {
        let mine: Vec<&(ChainFamily, NetworkId, Address)> =
            derived.iter().filter(|(of, ..)| *of == family).collect();
        let shared = mine.iter().all(|(.., address)| *address == mine[0].2);
        let account = |network: Option<NetworkId>, address: Address| Account {
            wallet,
            family,
            index,
            address,
            network,
        };
        if shared {
            accounts.push(account(None, mine[0].2));
        } else {
            accounts.extend(
                mine.into_iter()
                    .map(|(_, network, address)| account(Some(network.clone()), *address)),
            );
        }
    }
    accounts
}

pub(crate) struct Fetched {
    pub generation: u64,
    pub held: Vec<Held>,
    pub health: Vec<NetworkHealth>,
    pub scans: Vec<(Scan, Vec<Received>, Vec<Outgoing>)>,
}

struct NetworkFetch {
    held: Vec<Held>,
    health: NetworkHealth,
    scans: Vec<(Scan, Vec<Received>, Vec<Outgoing>)>,
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
    let listed: Vec<AssetId> = network.assets().map(|(asset, _)| asset).collect();
    let mut groups = vec![listed.clone()];
    let mut assets = listed;
    for token in custom
        .iter()
        .filter(|token| token.asset.network() == &network.id)
    {
        if !assets.contains(&token.asset) {
            assets.push(token.asset.clone());
            groups.push(vec![token.asset.clone()]);
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
        .filter(|account| account.serves(&network.id, chain.family()))
    {
        for group in &groups {
            match chain.balances(&account.address, group).await {
                Ok(balances) => result.held.extend(balances.into_iter().map(|balance| Held {
                    wallet: account.wallet,
                    address: account.address,
                    asset: balance.asset,
                    amount: balance.amount,
                    staked: balance.staked,
                })),
                Err(err) => result.health.problem = Some(err.to_string()),
            }
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

fn scan_of(
    network: &NetworkId,
    address: &Address,
    scanned: Scanned,
) -> (Scan, Vec<Received>, Vec<Outgoing>) {
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
            memo: incoming.memo,
        })
        .collect();
    let sent = scanned
        .sent
        .into_iter()
        .map(|outgoing: bahaa_chain::Outgoing| Outgoing {
            id: outgoing.id.to_string(),
            index: outgoing.index,
            network: network.clone(),
            from: outgoing.from,
            to: outgoing.to,
            asset: outgoing.asset,
            amount: outgoing.amount,
            block: outgoing.block,
            time: outgoing.time,
            memo: outgoing.memo,
        })
        .collect();
    (scan, found, sent)
}

pub(crate) fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_family_shares_one_account_unless_its_networks_disagree() {
        let wallet = crate::keyring::test_wallet(1);
        let id = |text: &str| -> NetworkId { text.parse().unwrap() };
        let evm = Address::Evm([1; 20]);
        let (main, test) = (
            Address::Ton {
                workchain: 0,
                hash: [2; 32],
            },
            Address::Ton {
                workchain: 0,
                hash: [3; 32],
            },
        );
        let derived = [
            (ChainFamily::Ton, id("ton"), main),
            (ChainFamily::Evm, id("bsc"), evm),
            (ChainFamily::Ton, id("ton-testnet"), test),
            (ChainFamily::Evm, id("bsc-testnet"), evm),
        ];
        let grouped: Vec<(ChainFamily, Option<NetworkId>, Address)> = accounts(wallet, 0, &derived)
            .into_iter()
            .map(|account| (account.family, account.network, account.address))
            .collect();
        assert_eq!(
            grouped,
            [
                (ChainFamily::Evm, None, evm),
                (ChainFamily::Ton, Some(id("ton")), main),
                (ChainFamily::Ton, Some(id("ton-testnet")), test),
            ]
        );
        let ton = accounts(wallet, 0, &derived)[1].clone();
        assert!(ton.serves(&id("ton"), ChainFamily::Ton));
        assert!(!ton.serves(&id("ton-testnet"), ChainFamily::Ton));
        assert!(!ton.serves(&id("ton"), ChainFamily::Evm));
    }
}
