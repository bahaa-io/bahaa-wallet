use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use bahaa_chain::ChainError;
use bahaa_config::{ChainKind, Network};
use bahaa_evm::{ens_coins, unstoppable_codes};
use bahaa_types::{Address, ChainFamily, NetworkId};

use super::{EngineError, Reply, Worker};
use crate::chains::{self, AnyChain};

const TRON_PREFIX: u8 = 0x41;
pub(super) const TON_ENDINGS: [&str; 2] = [".ton", ".t.me"];
pub(super) const SNS_ENDING: &str = ".sns";
pub(super) const PAUSED_ENDING: &str = ".sol";
const NO_ADDRESS: &str = "this name has no address on this network";
const CANT_USE: &str = "this name points to an address this network can't use";
const REMEMBER: Duration = Duration::from_secs(600);
const MAX_REMEMBERED: usize = 256;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Resolved {
    pub name: String,
    pub address: Address,
    pub service: NameService,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NameService {
    Ens,
    TonDns,
    Sns,
    SpaceId,
    Unstoppable,
    Bip353,
}

type Key = (NetworkId, Address);

#[derive(Clone)]
struct Seen {
    at: Instant,
    found: Option<Resolved>,
}

#[derive(Clone, Default)]
pub(super) struct Names(Arc<Mutex<HashMap<Key, Seen>>>);

impl Names {
    fn known(&self, key: &Key) -> Option<Seen> {
        let known = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        known
            .get(key)
            .filter(|seen| seen.at.elapsed() < REMEMBER)
            .cloned()
    }

    fn keep(&self, key: Key, found: Option<Resolved>) {
        let mut known = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        if known.len() >= MAX_REMEMBERED {
            known.clear();
        }
        let at = Instant::now();
        known.insert(key, Seen { at, found });
    }

    pub(super) fn forget(&self) {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clear();
    }
}

enum Lookup {
    Ens {
        ethereum: usize,
        coins: Vec<u64>,
    },
    TonDns {
        ton: usize,
    },
    Sns {
        solana: usize,
    },
    SpaceId {
        chain: usize,
    },
    Bip353 {
        bitcoin: usize,
    },
    Unstoppable {
        readers: Vec<usize>,
        codes: (&'static str, &'static str),
    },
}

impl Worker {
    pub(super) fn resolve_name(&self, name: &str, network: &NetworkId, reply: Reply<Resolved>) {
        let (lookup, target, name) = match self.name_lookup(name, network) {
            Ok(found) => found,
            Err(err) => {
                let _ = reply.send(Err(err));
                return;
            }
        };
        let chains = Arc::clone(&self.chains);
        tokio::spawn(async move {
            let result = match lookup {
                Lookup::Ens { ethereum, coins } => {
                    ens(&chains[ethereum], &target, &coins, name).await
                }
                Lookup::TonDns { ton } => native(
                    chains[ton].dns_address(&name).await,
                    name,
                    NameService::TonDns,
                ),
                Lookup::Sns { solana } => native(
                    chains[solana].sns_address(&name).await,
                    name,
                    NameService::Sns,
                ),
                Lookup::Bip353 { bitcoin } => native(
                    chains[bitcoin].bip353_address(&name).await,
                    name,
                    NameService::Bip353,
                ),
                Lookup::SpaceId { chain } => native(
                    chains[chain].space_id_address(&name).await,
                    name,
                    NameService::SpaceId,
                ),
                Lookup::Unstoppable { readers, codes } => {
                    unstoppable(&chains, &readers, &target, codes, name).await
                }
            };
            let _ = reply.send(result.map_err(EngineError::from));
        });
    }

    pub(super) fn name_of(
        &self,
        network: &NetworkId,
        address: Address,
        reply: Reply<Option<Resolved>>,
    ) {
        if !self.settings.name_lookups {
            let _ = reply.send(Ok(None));
            return;
        }
        let Some(index) = self.chain_index(network) else {
            let _ = reply.send(Err(EngineError::UnknownNetwork));
            return;
        };
        let key = (network.clone(), address);
        if let Some(seen) = self.names.known(&key) {
            let _ = reply.send(Ok(seen.found));
            return;
        }
        let target = self.chains[index].network().clone();
        let ethereum = chains::evm_chain(&self.chains, 1);
        let chains = Arc::clone(&self.chains);
        let names = self.names.clone();
        tokio::spawn(async move {
            let result = behind(&chains, index, ethereum, &target, &key.1).await;
            if let Ok(found) = &result {
                names.keep(key, found.clone());
            }
            let _ = reply.send(result.map_err(EngineError::from));
        });
    }

    fn name_lookup(
        &self,
        name: &str,
        network: &NetworkId,
    ) -> Result<(Lookup, Network, String), EngineError> {
        let index = self
            .chain_index(network)
            .ok_or(EngineError::UnknownNetwork)?;
        let target = self.chains[index].network().clone();
        let lower = name.trim().to_lowercase();
        let family = target.family();
        if lower.ends_with(PAUSED_ENDING) {
            return Err(ChainError::Invalid(
                "SNS has paused .sol names for now, use the .sns name",
            )
            .into());
        }
        if lower.contains('@') {
            if family != ChainFamily::Bitcoin || target.testnet {
                return Err(ChainError::Invalid(
                    "this is a Bitcoin name, it only points to Bitcoin addresses",
                )
                .into());
            }
            let name = bahaa_bitcoin::bip353_normalize(name)?;
            return Ok((Lookup::Bip353 { bitcoin: index }, target, name));
        }
        if TON_ENDINGS.iter().any(|ending| lower.ends_with(ending)) {
            if family != ChainFamily::Ton {
                return Err(ChainError::Invalid(
                    "this is a TON name, it only points to TON addresses",
                )
                .into());
            }
            let name = bahaa_ton::dns_normalize(name)?;
            return Ok((Lookup::TonDns { ton: index }, target, name));
        }
        if lower.ends_with(SNS_ENDING) {
            if family != ChainFamily::Solana || target.testnet {
                return Err(ChainError::Invalid(
                    "this is a Solana name, it only points to Solana mainnet addresses",
                )
                .into());
            }
            let name = bahaa_solana::sns_normalize(name)?;
            return Ok((Lookup::Sns { solana: index }, target, name));
        }
        if let Some((home, elsewhere)) = bahaa_evm::space_id_home(&lower) {
            if !matches!(target.kind, ChainKind::Evm { chain_id, .. } if chain_id == home) {
                return Err(ChainError::Invalid(elsewhere).into());
            }
            let name = bahaa_evm::ens_normalize(name)?;
            return Ok((Lookup::SpaceId { chain: index }, target, name));
        }
        if let Some(homes) = bahaa_evm::unstoppable_chains(&lower) {
            let codes = unstoppable_codes(&target).ok_or(ChainError::Unsupported("names yet"))?;
            let readers = homes
                .iter()
                .map(|home| chains::evm_chain(&self.chains, *home))
                .collect::<Option<Vec<_>>>()
                .ok_or(ChainError::Unsupported(
                    "names without Polygon, Ethereum and Base",
                ))?;
            let name = bahaa_evm::unstoppable_normalize(name)?;
            return Ok((Lookup::Unstoppable { readers, codes }, target, name));
        }
        let coins = ens_coins(&target).ok_or(ChainError::Unsupported("ENS names"))?;
        let ethereum = chains::evm_chain(&self.chains, 1)
            .ok_or(ChainError::Unsupported("names without Ethereum"))?;
        let name = bahaa_evm::ens_normalize(name)?;
        Ok((Lookup::Ens { ethereum, coins }, target, name))
    }
}

async fn behind(
    chains: &[AnyChain],
    index: usize,
    ethereum: Option<usize>,
    target: &Network,
    address: &Address,
) -> Result<Option<Resolved>, ChainError> {
    if target.testnet {
        return Ok(None);
    }
    match (target.family(), address) {
        (ChainFamily::Evm, Address::Evm(bytes)) => {
            let (Some(ethereum), Some(coins)) = (ethereum, ens_coins(target)) else {
                return Ok(None);
            };
            let Some(name) = chains[ethereum].ens_name(bytes, coins[0]).await? else {
                return Ok(None);
            };
            confirmed(ens(&chains[ethereum], target, &coins, name).await, address)
        }
        (ChainFamily::Ton, Address::Ton { .. }) => {
            for name in chains[index].dns_names(address).await? {
                let found = native(
                    chains[index].dns_address(&name).await,
                    name,
                    NameService::TonDns,
                );
                if let Some(resolved) = confirmed(found, address)? {
                    return Ok(Some(resolved));
                }
            }
            Ok(None)
        }
        _ => Ok(None),
    }
}

fn confirmed(
    found: Result<Resolved, ChainError>,
    address: &Address,
) -> Result<Option<Resolved>, ChainError> {
    match found {
        Ok(resolved) => Ok((resolved.address == *address).then_some(resolved)),
        Err(ChainError::Invalid(_)) => Ok(None),
        Err(err) => Err(err),
    }
}

fn native(
    found: Result<Option<Address>, ChainError>,
    name: String,
    service: NameService,
) -> Result<Resolved, ChainError> {
    let address = usable(found?.ok_or(ChainError::Invalid(NO_ADDRESS))?)?;
    Ok(Resolved {
        name,
        address,
        service,
    })
}

async fn ens(
    ethereum: &AnyChain,
    target: &Network,
    coins: &[u64],
    name: String,
) -> Result<Resolved, ChainError> {
    for coin in coins {
        if let Some(bytes) = ethereum.ens_address(&name, *coin).await? {
            let address = address_of(target, &bytes).ok_or(ChainError::Invalid(CANT_USE))?;
            return Ok(Resolved {
                name,
                address,
                service: NameService::Ens,
            });
        }
    }
    Err(ChainError::Invalid(NO_ADDRESS))
}

async fn unstoppable(
    chains: &[AnyChain],
    readers: &[usize],
    target: &Network,
    (network, token): (&str, &str),
    name: String,
) -> Result<Resolved, ChainError> {
    for reader in readers {
        if let Some(text) = chains[*reader]
            .unstoppable_address(&name, network, token)
            .await?
        {
            let address = Address::parse(target.family(), &text)
                .ok()
                .filter(|address| !matches!(address, Address::Bitcoin { testnet: true, .. }))
                .ok_or(ChainError::Invalid(CANT_USE))?;
            let address = usable(address)?;
            return Ok(Resolved {
                name,
                address,
                service: NameService::Unstoppable,
            });
        }
    }
    Err(ChainError::Invalid(NO_ADDRESS))
}

fn usable(address: Address) -> Result<Address, ChainError> {
    let blank = match &address {
        Address::Evm(bytes) | Address::Tron(bytes) => *bytes == [0; 20],
        Address::Solana(bytes) => *bytes == [0; 32],
        Address::Ton { hash, .. } => *hash == [0; 32],
        Address::Bitcoin { .. } => false,
    };
    if blank {
        Err(ChainError::Invalid(NO_ADDRESS))
    } else {
        Ok(address)
    }
}

fn address_of(network: &Network, bytes: &[u8]) -> Option<Address> {
    match network.family() {
        ChainFamily::Evm => Some(Address::Evm(bytes.try_into().ok()?)),
        ChainFamily::Solana => Some(Address::Solana(bytes.try_into().ok()?)),
        ChainFamily::Tron => match bytes {
            [TRON_PREFIX, rest @ ..] => Some(Address::Tron(rest.try_into().ok()?)),
            _ => None,
        },
        ChainFamily::Bitcoin => Some(Address::Bitcoin {
            script: bahaa_bitcoin::read_script(bytes)?,
            testnet: false,
        }),
        ChainFamily::Ton => None,
    }
}

#[cfg(test)]
mod tests {
    use bahaa_config::Registry;

    use super::*;

    fn network(id: &str) -> Network {
        Registry::builtin()
            .unwrap()
            .get(&id.parse().unwrap())
            .unwrap()
            .clone()
    }

    #[test]
    fn stored_addresses_become_addresses_of_the_asked_network() {
        let evm = [0xd8; 20];
        assert_eq!(address_of(&network("base"), &evm), Some(Address::Evm(evm)));
        assert_eq!(address_of(&network("base"), &[1; 21]), None);
        let mut tron = vec![0x41];
        tron.extend([7; 20]);
        assert_eq!(
            address_of(&network("tron"), &tron),
            Some(Address::Tron([7; 20]))
        );
        assert_eq!(address_of(&network("tron"), &[7; 21]), None);
        assert_eq!(
            address_of(&network("solana"), &[9; 32]),
            Some(Address::Solana([9; 32]))
        );
        let mut segwit = vec![0x00, 0x14];
        segwit.extend([3; 20]);
        assert_eq!(
            address_of(&network("bitcoin"), &segwit),
            Some(Address::Bitcoin {
                script: bahaa_types::BitcoinScript::WitnessKeyHash([3; 20]),
                testnet: false,
            })
        );
        assert_eq!(address_of(&network("bitcoin"), &[0x6a, 1, 2]), None);
    }
}
