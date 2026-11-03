use std::collections::BTreeMap;

use bahaa_types::{AssetId, NetworkId};
use serde::Deserialize;
use url::Url;

use crate::order::SwapError;

const BUILTIN: &str = include_str!("providers.json");

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Providers {
    pub oneclick: OneClickConfig,
    pub relay: RelayConfig,
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
    #[serde(rename = "none")]
    Unsupported,
}

impl Providers {
    pub fn builtin() -> Result<Self, SwapError> {
        serde_json::from_str(BUILTIN)
            .map_err(|_| SwapError::Untrusted("the built-in swap provider list is broken"))
    }
}
