use std::sync::Arc;

use bahaa_chain::ChainError;
use bahaa_config::{ChainKind, Network};
use bahaa_types::{Address, ChainFamily, NetworkId};

use super::{EngineError, Reply, Worker};
use crate::chains::{self, AnyChain};

const ETHEREUM: u64 = 60;
const EVM: u64 = 0x8000_0000;
const BITCOIN: u64 = 0;
const SOLANA: u64 = 501;
const TRON: u64 = 195;
const TRON_PREFIX: u8 = 0x41;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Resolved {
    pub name: String,
    pub address: Address,
    pub service: NameService,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NameService {
    Ens,
}

impl Worker {
    pub(super) fn resolve_name(&self, name: &str, network: &NetworkId, reply: Reply<Resolved>) {
        let found = self.name_target(name, network);
        let (target, coins, ethereum, name) = match found {
            Ok(found) => found,
            Err(err) => {
                let _ = reply.send(Err(err));
                return;
            }
        };
        let chains = Arc::clone(&self.chains);
        tokio::spawn(async move {
            let result = ens(&chains[ethereum], &target, &coins, name).await;
            let _ = reply.send(result.map_err(EngineError::from));
        });
    }

    fn name_target(
        &self,
        name: &str,
        network: &NetworkId,
    ) -> Result<(Network, Vec<u64>, usize, String), EngineError> {
        let target = self
            .chain_index(network)
            .map(|index| self.chains[index].network().clone())
            .ok_or(EngineError::UnknownNetwork)?;
        let coins = coin_types(&target).ok_or(ChainError::Unsupported("names yet"))?;
        let ethereum = chains::ethereum_mainnet(&self.chains)
            .ok_or(ChainError::Unsupported("names without Ethereum"))?;
        Ok((target, coins, ethereum, bahaa_evm::ens_normalize(name)?))
    }
}

async fn ens(
    ethereum: &AnyChain,
    target: &Network,
    coins: &[u64],
    name: String,
) -> Result<Resolved, ChainError> {
    for coin in coins {
        if let Some(bytes) = ethereum.ens_address(&name, *coin).await? {
            let address = address_of(target, &bytes).ok_or(ChainError::Invalid(
                "this name points to an address this network can't use",
            ))?;
            return Ok(Resolved {
                name,
                address,
                service: NameService::Ens,
            });
        }
    }
    Err(ChainError::Invalid(
        "this name has no address on this network",
    ))
}

fn coin_types(network: &Network) -> Option<Vec<u64>> {
    if network.testnet {
        return None;
    }
    match network.kind {
        ChainKind::Evm { chain_id: 1, .. } => Some(vec![ETHEREUM]),
        ChainKind::Evm { chain_id, .. } if chain_id < EVM => Some(vec![EVM | chain_id, EVM]),
        ChainKind::Bitcoin { .. } => Some(vec![BITCOIN]),
        ChainKind::Solana { .. } => Some(vec![SOLANA]),
        ChainKind::Tron { .. } => Some(vec![TRON]),
        ChainKind::Evm { .. } | ChainKind::Ton { .. } => None,
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
    fn each_network_asks_for_its_own_record_and_evm_networks_fall_back_to_the_default() {
        assert_eq!(coin_types(&network("ethereum")), Some(vec![60]));
        assert_eq!(
            coin_types(&network("base")),
            Some(vec![0x8000_2105, 0x8000_0000])
        );
        assert_eq!(
            coin_types(&network("bsc")),
            Some(vec![0x8000_0038, 0x8000_0000])
        );
        assert_eq!(coin_types(&network("bitcoin")), Some(vec![0]));
        assert_eq!(coin_types(&network("solana")), Some(vec![501]));
        assert_eq!(coin_types(&network("tron")), Some(vec![195]));
        assert_eq!(coin_types(&network("ton")), None);
        assert_eq!(coin_types(&network("bsc-testnet")), None);
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
