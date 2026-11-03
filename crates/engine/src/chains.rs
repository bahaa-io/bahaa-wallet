use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::time::Duration;

use bahaa_bitcoin::{BitcoinChain, BitcoinPlan};
use bahaa_chain::{
    Approval, Balance, Chain, ChainError, ContractSwap, Decoded, DecodedCall, FeeQuote, Movement,
    PlannedCall, Replace, ScanCursor, Scanned, Signed, Transfer, TxId, TxStatus,
};
use bahaa_config::{Coin, Network, Registry};
use bahaa_crypto::Seed;
use bahaa_evm::{EvmCall, EvmChain, EvmPlan};
use bahaa_solana::{SolanaCall, SolanaChain, SolanaPlan};
use bahaa_store::{CustomToken, Outgoing, Received, Scan, TrustedBlock};
use bahaa_ton::{TonCall, TonChain, TonPlan};
use bahaa_tron::{TronChain, TronPlan};
use bahaa_types::{Address, Amount, AssetId, ChainFamily, NetworkId, TonContract};

use crate::WalletInfo;
use futures_util::FutureExt;
use futures_util::future::join_all;

use crate::portfolio::{Account, Held};
use crate::{WalletId, WalletKind};

pub(crate) enum AnyChain {
    Evm(EvmChain),
    Tron(TronChain),
    Ton(Box<TonChain>),
    Solana(SolanaChain),
    Bitcoin(Box<BitcoinChain>),
}

#[derive(Clone)]
pub(crate) enum AnyCall {
    Evm(EvmCall),
    Solana(SolanaCall),
    Ton(Box<TonCall>),
}

impl AnyCall {
    pub(crate) fn planned(&self) -> &PlannedCall {
        match self {
            Self::Evm(call) => call.planned(),
            Self::Solana(call) => call.planned(),
            Self::Ton(call) => call.planned(),
        }
    }

    pub(crate) fn fee_max(&self) -> Amount {
        match self {
            Self::Evm(call) => call.fee_max(),
            Self::Solana(call) => call.fee_max(),
            Self::Ton(call) => call.fee_max(),
        }
    }

    pub(crate) fn checked(&self) -> bool {
        match self {
            Self::Evm(call) => call.checked(),
            Self::Solana(_) => true,
            Self::Ton(_) => false,
        }
    }
}

#[derive(Clone)]
pub(crate) enum AnyPlan {
    Evm(EvmPlan),
    Tron(TronPlan),
    Ton(TonPlan),
    Solana(SolanaPlan),
    Bitcoin(BitcoinPlan),
}

impl AnyChain {
    pub(crate) fn network(&self) -> &Network {
        match self {
            Self::Evm(chain) => chain.network(),
            Self::Tron(chain) => chain.network(),
            Self::Ton(chain) => chain.network(),
            Self::Solana(chain) => chain.network(),
            Self::Bitcoin(chain) => chain.network(),
        }
    }

    pub(crate) fn family(&self) -> ChainFamily {
        self.network().family()
    }

    pub(crate) fn holds(&self, kind: WalletKind) -> bool {
        match kind {
            WalletKind::Bip39 => true,
            WalletKind::Ton => matches!(self, Self::Ton(_)),
            WalletKind::Key(family) | WalletKind::Watch(family) => self.family() == family,
        }
    }

    pub(crate) fn derive(
        &self,
        seed: &Seed,
        account: u32,
        wallet: &WalletInfo,
    ) -> Result<Address, ChainError> {
        match self {
            Self::Evm(_) => bahaa_evm::derive(seed, account),
            Self::Tron(_) => bahaa_tron::derive(seed, account),
            Self::Ton(chain) => {
                chain.derive_as(seed, account, wallet.ton.unwrap_or(TonContract::W5))
            }
            Self::Solana(_) => {
                bahaa_solana::derive(seed, account, wallet.solana.unwrap_or_default())
            }
            Self::Bitcoin(chain) => {
                chain.derive_as(seed, account, wallet.bitcoin.unwrap_or_default())
            }
        }
    }

    pub(crate) fn evm(&self) -> Option<&EvmChain> {
        match self {
            Self::Evm(chain) => Some(chain),
            Self::Tron(_) | Self::Ton(_) | Self::Solana(_) | Self::Bitcoin(_) => None,
        }
    }

    pub(crate) fn ton(&self) -> Option<&TonChain> {
        match self {
            Self::Ton(chain) => Some(chain),
            Self::Evm(_) | Self::Tron(_) | Self::Solana(_) | Self::Bitcoin(_) => None,
        }
    }

    pub(crate) fn solana(&self) -> Option<&SolanaChain> {
        match self {
            Self::Solana(chain) => Some(chain),
            Self::Evm(_) | Self::Tron(_) | Self::Ton(_) | Self::Bitcoin(_) => None,
        }
    }

    pub(crate) async fn prepare_swap(
        &self,
        swap: &ContractSwap,
    ) -> Result<(Vec<AnyCall>, FeeQuote), ChainError> {
        shielded(async {
            match self {
                Self::Evm(chain) => {
                    let (plans, fee) = chain.prepare_swap(swap).await?;
                    Ok((plans.into_iter().map(AnyCall::Evm).collect(), fee))
                }
                Self::Solana(chain) => {
                    let (plans, fee) = chain.prepare_swap(swap).await?;
                    Ok((plans.into_iter().map(AnyCall::Solana).collect(), fee))
                }
                Self::Ton(chain) => {
                    let (plans, fee) = chain.prepare_swap(swap).await?;
                    Ok((
                        plans
                            .into_iter()
                            .map(|plan| AnyCall::Ton(Box::new(plan)))
                            .collect(),
                        fee,
                    ))
                }
                Self::Tron(_) | Self::Bitcoin(_) => {
                    Err(ChainError::Unsupported("swaps through contracts"))
                }
            }
        })
        .await
    }

    pub(crate) async fn prepare_approval(
        &self,
        approval: &Approval,
    ) -> Result<(Vec<AnyCall>, FeeQuote), ChainError> {
        shielded(async {
            match self {
                Self::Evm(chain) => {
                    let (plans, fee) = chain.prepare_approval(approval).await?;
                    Ok((plans.into_iter().map(AnyCall::Evm).collect(), fee))
                }
                Self::Tron(_) | Self::Ton(_) | Self::Solana(_) | Self::Bitcoin(_) => {
                    Err(ChainError::Unsupported("swaps through contracts"))
                }
            }
        })
        .await
    }

    pub(crate) fn sign_typed(
        &self,
        digest: &[u8; 32],
        seed: &Seed,
        account: u32,
        from: &Address,
    ) -> Result<[u8; 65], ChainError> {
        match self {
            Self::Evm(chain) => chain.sign_typed(digest, seed, account, from),
            Self::Tron(_) | Self::Ton(_) | Self::Solana(_) | Self::Bitcoin(_) => {
                Err(ChainError::Unsupported("signed orders"))
            }
        }
    }

    pub(crate) async fn recheck(
        &self,
        swap: &ContractSwap,
        calls: &[AnyCall],
    ) -> Result<(), ChainError> {
        shielded(async {
            match self {
                Self::Evm(chain) => {
                    let plans: Vec<EvmCall> = calls
                        .iter()
                        .filter_map(|call| match call {
                            AnyCall::Evm(call) => Some(call.clone()),
                            AnyCall::Solana(_) | AnyCall::Ton(_) => None,
                        })
                        .collect();
                    if plans.len() != calls.len() {
                        return Err(ChainError::Mismatch("the network"));
                    }
                    chain.recheck(swap, &plans).await
                }
                Self::Solana(chain) => chain.recheck(swap).await,
                Self::Ton(chain) => chain.recheck(swap).await,
                Self::Tron(_) | Self::Bitcoin(_) => {
                    Err(ChainError::Unsupported("swaps through contracts"))
                }
            }
        })
        .await
    }

    pub(crate) fn sign_call(&self, plan: &AnyCall, seed: &Seed) -> Result<Signed, ChainError> {
        match (self, plan) {
            (Self::Evm(chain), AnyCall::Evm(plan)) => chain.sign_call(plan, seed),
            (Self::Solana(chain), AnyCall::Solana(plan)) => chain.sign_call(plan, seed),
            (Self::Ton(chain), AnyCall::Ton(plan)) => chain.sign_call(plan, seed),
            _ => Err(ChainError::Mismatch("the network")),
        }
    }

    pub(crate) fn decode_call(&self, signed: &Signed) -> Result<DecodedCall, ChainError> {
        std::panic::catch_unwind(AssertUnwindSafe(|| match self {
            Self::Evm(chain) => chain.decode_call(signed),
            Self::Solana(chain) => chain.decode_call(signed),
            Self::Ton(chain) => chain.decode_call(signed),
            Self::Tron(_) | Self::Bitcoin(_) => {
                Err(ChainError::Unsupported("swaps through contracts"))
            }
        }))
        .unwrap_or_else(|_| Err(unreadable()))
    }

    pub(crate) async fn broadcast_private(&self, signed: &Signed) -> Result<TxId, ChainError> {
        match self {
            Self::Evm(chain) => shielded(chain.broadcast_private(signed)).await,
            Self::Tron(_) | Self::Ton(_) | Self::Solana(_) | Self::Bitcoin(_) => {
                self.broadcast(signed).await
            }
        }
    }

    pub(crate) fn forget(&self) {
        if let Self::Bitcoin(chain) = self {
            chain.forget();
        }
    }

    pub(crate) async fn receive_address(&self, address: &Address) -> Result<Address, ChainError> {
        match self {
            Self::Bitcoin(chain) => shielded(chain.receive_address(address)).await,
            Self::Evm(_) | Self::Tron(_) | Self::Ton(_) | Self::Solana(_) => Ok(*address),
        }
    }

    pub(crate) fn quote_life(&self) -> Option<Duration> {
        match self {
            Self::Solana(_) => Some(bahaa_solana::QUOTE_LIFE),
            Self::Evm(_) | Self::Tron(_) | Self::Ton(_) | Self::Bitcoin(_) => None,
        }
    }

    async fn balances(
        &self,
        address: &Address,
        assets: &[AssetId],
    ) -> Result<Vec<Balance>, ChainError> {
        shielded(async {
            match self {
                Self::Evm(chain) => chain.balances(address, assets).await,
                Self::Tron(chain) => chain.balances(address, assets).await,
                Self::Ton(chain) => chain.balances(address, assets).await,
                Self::Solana(chain) => chain.balances(address, assets).await,
                Self::Bitcoin(chain) => chain.balances(address, assets).await,
            }
        })
        .await
    }

    pub(crate) async fn token(&self, contract: &Address) -> Result<Coin, ChainError> {
        shielded(async {
            match self {
                Self::Evm(chain) => chain.token(contract).await,
                Self::Tron(chain) => chain.token(contract).await,
                Self::Ton(chain) => chain.token(contract).await,
                Self::Solana(chain) => chain.token(contract).await,
                Self::Bitcoin(chain) => chain.token(contract).await,
            }
        })
        .await
    }

    pub(crate) fn know(&self, contract: &Address, decimals: u8) {
        match self {
            Self::Evm(chain) => chain.know(contract, decimals),
            Self::Tron(chain) => chain.know(contract, decimals),
            Self::Ton(chain) => chain.know(contract, decimals),
            Self::Solana(chain) => chain.know(contract, decimals),
            Self::Bitcoin(chain) => chain.know(contract, decimals),
        }
    }

    async fn scan(
        &self,
        address: &Address,
        assets: &[AssetId],
        cursor: Option<ScanCursor>,
    ) -> Result<Option<Scanned>, ChainError> {
        shielded(async {
            match self {
                Self::Evm(chain) if chain.can_scan() => chain.scan(address, assets, cursor).await,
                Self::Tron(chain) if chain.can_scan() => chain.scan(address, assets, cursor).await,
                Self::Ton(chain) if chain.can_scan() => chain.scan(address, assets, cursor).await,
                Self::Solana(chain) if chain.can_scan() => {
                    chain.scan(address, assets, cursor).await
                }
                Self::Bitcoin(chain) if chain.can_scan() => {
                    chain.scan(address, assets, cursor).await
                }
                Self::Evm(_)
                | Self::Tron(_)
                | Self::Ton(_)
                | Self::Solana(_)
                | Self::Bitcoin(_) => Ok(None),
            }
        })
        .await
    }

    pub(crate) async fn prepare(
        &self,
        transfer: &Transfer,
    ) -> Result<(AnyPlan, Transfer, FeeQuote), ChainError> {
        shielded(async {
            match self {
                Self::Evm(chain) => {
                    Ok(planned(chain, chain.prepare(transfer).await?, AnyPlan::Evm))
                }
                Self::Tron(chain) => Ok(planned(
                    chain,
                    chain.prepare(transfer).await?,
                    AnyPlan::Tron,
                )),
                Self::Ton(chain) => Ok(planned(
                    &**chain,
                    chain.prepare(transfer).await?,
                    AnyPlan::Ton,
                )),
                Self::Solana(chain) => Ok(planned(
                    chain,
                    chain.prepare(transfer).await?,
                    AnyPlan::Solana,
                )),
                Self::Bitcoin(chain) => Ok(planned(
                    &**chain,
                    chain.prepare(transfer).await?,
                    AnyPlan::Bitcoin,
                )),
            }
        })
        .await
    }

    pub(crate) async fn replace(
        &self,
        original: &Signed,
        account: u32,
        how: Replace,
    ) -> Result<(AnyPlan, Transfer, FeeQuote), ChainError> {
        shielded(async {
            match self {
                Self::Evm(chain) => Ok(planned(
                    chain,
                    chain.replace(original, account, how).await?,
                    AnyPlan::Evm,
                )),
                Self::Tron(chain) => Ok(planned(
                    chain,
                    chain.replace(original, account, how).await?,
                    AnyPlan::Tron,
                )),
                Self::Ton(chain) => Ok(planned(
                    &**chain,
                    chain.replace(original, account, how).await?,
                    AnyPlan::Ton,
                )),
                Self::Solana(chain) => Ok(planned(
                    chain,
                    chain.replace(original, account, how).await?,
                    AnyPlan::Solana,
                )),
                Self::Bitcoin(chain) => Ok(planned(
                    &**chain,
                    chain.replace(original, account, how).await?,
                    AnyPlan::Bitcoin,
                )),
            }
        })
        .await
    }

    pub(crate) fn sign(&self, plan: &AnyPlan, seed: &Seed) -> Result<Signed, ChainError> {
        match (self, plan) {
            (Self::Evm(chain), AnyPlan::Evm(plan)) => chain.sign(plan, seed),
            (Self::Tron(chain), AnyPlan::Tron(plan)) => chain.sign(plan, seed),
            (Self::Ton(chain), AnyPlan::Ton(plan)) => chain.sign(plan, seed),
            (Self::Solana(chain), AnyPlan::Solana(plan)) => chain.sign(plan, seed),
            (Self::Bitcoin(chain), AnyPlan::Bitcoin(plan)) => chain.sign(plan, seed),
            _ => Err(ChainError::Mismatch("the network")),
        }
    }

    pub(crate) fn decode(&self, signed: &Signed) -> Result<Decoded, ChainError> {
        std::panic::catch_unwind(AssertUnwindSafe(|| match self {
            Self::Evm(chain) => chain.decode(signed),
            Self::Tron(chain) => chain.decode(signed),
            Self::Ton(chain) => chain.decode(signed),
            Self::Solana(chain) => chain.decode(signed),
            Self::Bitcoin(chain) => chain.decode(signed),
        }))
        .unwrap_or_else(|_| Err(unreadable()))
    }

    pub(crate) async fn broadcast(&self, signed: &Signed) -> Result<TxId, ChainError> {
        shielded(async {
            match self {
                Self::Evm(chain) => chain.broadcast(signed).await,
                Self::Tron(chain) => chain.broadcast(signed).await,
                Self::Ton(chain) => chain.broadcast(signed).await,
                Self::Solana(chain) => chain.broadcast(signed).await,
                Self::Bitcoin(chain) => chain.broadcast(signed).await,
            }
        })
        .await
    }

    pub(crate) async fn status(
        &self,
        id: &TxId,
        from: &Address,
        expires_at: Option<u64>,
    ) -> Result<TxStatus, ChainError> {
        shielded(async {
            match self {
                Self::Evm(chain) => chain.status(id, from, expires_at).await,
                Self::Tron(chain) => chain.status(id, from, expires_at).await,
                Self::Ton(chain) => chain.status(id, from, expires_at).await,
                Self::Solana(chain) => chain.status(id, from, expires_at).await,
                Self::Bitcoin(chain) => chain.status(id, from, expires_at).await,
            }
        })
        .await
    }
}

fn planned<C: Chain>(
    chain: &C,
    (plan, fee): (C::Plan, FeeQuote),
    wrap: fn(C::Plan) -> AnyPlan,
) -> (AnyPlan, Transfer, FeeQuote) {
    let planned = chain.planned(&plan).clone();
    (wrap(plan), planned, fee)
}

async fn shielded<T>(work: impl Future<Output = Result<T, ChainError>>) -> Result<T, ChainError> {
    AssertUnwindSafe(work)
        .catch_unwind()
        .await
        .unwrap_or_else(|_| Err(unreadable()))
}

fn unreadable() -> ChainError {
    ChainError::Network("an endpoint sent a reply this wallet cannot read".to_owned())
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
            ChainFamily::Solana => SolanaChain::new(network.clone()).ok().map(AnyChain::Solana),
            ChainFamily::Bitcoin => BitcoinChain::new(network.clone())
                .ok()
                .map(|chain| AnyChain::Bitcoin(Box::new(chain))),
        })
        .collect()
}

pub(crate) fn ton_mainnet(chains: &[AnyChain]) -> Option<&TonChain> {
    chains.iter().find_map(|chain| match chain {
        AnyChain::Ton(ton) if !ton.network().testnet => Some(&**ton),
        _ => None,
    })
}

pub(crate) fn solana_mainnet(chains: &[AnyChain]) -> Option<&SolanaChain> {
    chains.iter().find_map(|chain| match chain {
        AnyChain::Solana(solana) if !solana.network().testnet => Some(solana),
        _ => None,
    })
}

pub(crate) fn bitcoin_mainnet(chains: &[AnyChain]) -> Option<&BitcoinChain> {
    chains.iter().find_map(|chain| match chain {
        AnyChain::Bitcoin(bitcoin) if !bitcoin.network().testnet => Some(&**bitcoin),
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
            AnyChain::Evm(_) | AnyChain::Tron(_) | AnyChain::Solana(_) | AnyChain::Bitcoin(_) => {
                None
            }
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

pub(crate) struct NetworkBalances {
    pub network: NetworkId,
    pub held: Vec<Held>,
    pub problem: Option<String>,
    pub checked_at: u64,
}

pub(crate) struct NetworkHistory {
    pub network: NetworkId,
    pub scans: Vec<(Scan, Vec<Received>, Vec<Outgoing>)>,
    pub problem: Option<String>,
}

pub(crate) async fn balances(
    chain: &AnyChain,
    accounts: &[Account],
    custom: &[CustomToken],
) -> NetworkBalances {
    let network = chain.network();
    let (_, groups) = assets_of(network, custom);
    let groups = &groups;
    let reads = served(chain, accounts).map(|account| async move {
        let mut held = Vec::new();
        let mut problem = None;
        for group in groups {
            match chain.balances(&account.address, group).await {
                Ok(balances) => held.extend(balances.into_iter().map(|balance| Held {
                    wallet: account.wallet,
                    address: account.address,
                    asset: balance.asset,
                    amount: balance.amount,
                    staked: balance.staked,
                })),
                Err(err) => problem = Some(err.to_string()),
            }
        }
        (held, problem)
    });
    let mut result = NetworkBalances {
        network: network.id.clone(),
        held: Vec::new(),
        problem: None,
        checked_at: 0,
    };
    for (held, problem) in join_all(reads).await {
        result.held.extend(held);
        result.problem = result.problem.or(problem);
    }
    result.checked_at = bahaa_chain::now();
    result
}

pub(crate) async fn history(
    chain: &AnyChain,
    accounts: &[Account],
    custom: &[CustomToken],
    cursors: &[Scan],
) -> NetworkHistory {
    let network = chain.network();
    let (assets, _) = assets_of(network, custom);
    let mut result = NetworkHistory {
        network: network.id.clone(),
        scans: Vec::new(),
        problem: None,
    };
    for account in served(chain, accounts) {
        let cursor = cursors
            .iter()
            .find(|scan| scan.network == network.id && scan.address == account.address)
            .map(cursor_of);
        match chain.scan(&account.address, &assets, cursor).await {
            Ok(Some(scanned)) => result
                .scans
                .push(scan_of(&network.id, &account.address, scanned)),
            Ok(None) => {}
            Err(err) => result.problem = Some(err.to_string()),
        }
    }
    result
}

fn served<'a>(chain: &'a AnyChain, accounts: &'a [Account]) -> impl Iterator<Item = &'a Account> {
    accounts
        .iter()
        .filter(|account| account.serves(&chain.network().id, chain.family()))
}

fn assets_of(network: &Network, custom: &[CustomToken]) -> (Vec<AssetId>, Vec<Vec<AssetId>>) {
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
    (assets, groups)
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
    let kept = |moved: Movement| bahaa_store::Movement {
        id: moved.id.to_string(),
        index: moved.index,
        network: network.clone(),
        from: moved.from,
        to: moved.to,
        asset: moved.asset,
        amount: moved.amount,
        block: moved.block,
        time: moved.time,
        memo: moved.memo,
    };
    let found = scanned.found.into_iter().map(kept).collect();
    let sent = scanned.sent.into_iter().map(kept).collect();
    (scan, found, sent)
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

    #[tokio::test]
    async fn a_reader_that_panics_fails_only_its_own_call() {
        let hostile: Result<(), ChainError> = shielded(async { panic!("a hostile reply") }).await;
        assert!(matches!(hostile, Err(ChainError::Network(_))));
        let fine = shielded(async { Ok::<_, ChainError>(7) }).await;
        assert_eq!(fine, Ok(7));
    }
}
