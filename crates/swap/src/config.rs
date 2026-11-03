use std::collections::{BTreeMap, BTreeSet};

use bahaa_types::{AssetId, NetworkId};
use serde::Deserialize;
use url::Url;

use crate::order::{Provider, SwapError};

const BUILTIN: &str = include_str!("providers.json");

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Providers {
    pub oneclick: OneClickConfig,
    pub relay: RelayConfig,
    pub pools: BTreeMap<NetworkId, PoolConfig>,
    pub kyberswap: KyberConfig,
    pub across: AcrossConfig,
    pub velora: VeloraConfig,
    pub cow: CowConfig,
    pub jupiter: JupiterConfig,
    pub stonfi: StonFiConfig,
    pub sunswap: SunSwapConfig,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SunSwapConfig {
    pub network: NetworkId,
    pub router: String,
    pub wrapped: String,
    pub fee_bps: u32,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StonFiConfig {
    pub url: Url,
    pub network: NetworkId,
    pub routers: Vec<StonFiRouter>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StonFiRouter {
    pub address: String,
    pub version: u8,
    pub pton: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JupiterConfig {
    pub url: Url,
    pub network: NetworkId,
    pub program: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CowConfig {
    pub url: Url,
    pub settlement: String,
    pub relayer: String,
    pub networks: BTreeMap<NetworkId, CowNetwork>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CowNetwork {
    pub path: String,
    pub chain: u64,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VeloraConfig {
    pub url: Url,
    pub augustus: String,
    pub chains: BTreeMap<NetworkId, u64>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AcrossConfig {
    pub url: Url,
    pub spokes: BTreeMap<NetworkId, SpokeConfig>,
    pub assets: BTreeMap<AssetId, String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpokeConfig {
    pub chain: u64,
    pub address: String,
    pub wrapped: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KyberConfig {
    pub url: Url,
    pub router: String,
    pub chains: BTreeMap<NetworkId, String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PoolConfig {
    pub provider: Provider,
    pub quoter: String,
    pub router: String,
    pub wrapped: String,
    pub fees: Vec<u32>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OneClickConfig {
    pub url: Url,
    pub key: String,
    pub assets: BTreeMap<AssetId, String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RelayConfig {
    pub url: Url,
    pub chains: BTreeMap<NetworkId, RelayChain>,
    pub assets: BTreeSet<AssetId>,
    pub contracts: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RelayChain {
    pub id: u64,
    pub deposit: RelayDeposit,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RelayDeposit {
    Address,
    Comment,
    Call,
    #[serde(rename = "none")]
    Unsupported,
}

impl Providers {
    pub fn builtin() -> Result<Self, SwapError> {
        serde_json::from_str(BUILTIN)
            .map_err(|_| SwapError::Untrusted("the built-in swap provider list is broken"))
    }
}
