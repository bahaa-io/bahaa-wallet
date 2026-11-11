use serde::{Deserialize, Serialize};

use crate::{Address, Amount, AssetId};

pub const MAX_NFTS: usize = 256;
pub const MAX_NFT_TEXT: usize = 120;
pub const MAX_NFT_LINK: usize = 2_048;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum NftKind {
    Erc721,
    Erc1155,
    TonItem,
    SolanaToken,
    SolanaProgrammable,
    SolanaCore,
    SolanaCompressed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Trust {
    Trusted,
    Unknown,
    Flagged,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Lock {
    Soulbound,
    Frozen,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Collection {
    pub address: Option<Address>,
    pub name: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Nft {
    pub asset: AssetId,
    pub kind: NftKind,
    pub amount: Amount,
    pub collection: Option<Collection>,
    pub name: Option<String>,
    pub link: Option<String>,
    pub trust: Trust,
    pub lock: Option<Lock>,
}
