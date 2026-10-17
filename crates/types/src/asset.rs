use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::address::TON_FRIENDLY_LEN;
use crate::{Address, AddressError, ChainFamily, IdError, NetworkId};

#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub enum AssetId {
    Native(NetworkId),
    Token(NetworkId, Address),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
pub enum AssetError {
    #[error(transparent)]
    Network(#[from] IdError),
    #[error(transparent)]
    Contract(#[from] AddressError),
}

impl AssetId {
    pub fn network(&self) -> &NetworkId {
        match self {
            Self::Native(network) | Self::Token(network, _) => network,
        }
    }

    pub fn contract(&self) -> Option<&Address> {
        match self {
            Self::Native(_) => None,
            Self::Token(_, contract) => Some(contract),
        }
    }
}

impl FromStr for AssetId {
    type Err = AssetError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let Some((network, contract)) = text.split_once('/') else {
            return Ok(Self::Native(text.parse()?));
        };
        let family = if contract.starts_with("0x") {
            ChainFamily::Evm
        } else if contract.contains(':') || contract.len() == TON_FRIENDLY_LEN {
            ChainFamily::Ton
        } else {
            ChainFamily::Tron
        };
        Ok(Self::Token(
            network.parse()?,
            Address::parse(family, contract)?,
        ))
    }
}

impl TryFrom<String> for AssetId {
    type Error = AssetError;

    fn try_from(text: String) -> Result<Self, Self::Error> {
        text.parse()
    }
}

impl From<AssetId> for String {
    fn from(id: AssetId) -> Self {
        id.to_string()
    }
}

impl fmt::Display for AssetId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Native(network) => write!(f, "{network}"),
            Self::Token(network, contract) => write!(f, "{network}/{contract}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_and_token_ids_round_trip() {
        for text in [
            "bsc",
            "bsc/0x55d398326f99059fF775485246999027B3197955",
            "tron/TR7NHqjeKQxGTCi8q8ZY4pL8otSzgjLj6t",
            "ton/0:62f9d7e6635da5377c4b1386f5f82de306f3ac20819012ba3790a9db6e6eaa58",
        ] {
            let id: AssetId = text.parse().unwrap();
            assert_eq!(id.to_string(), text);
            let json = serde_json::to_string(&id).unwrap();
            assert_eq!(serde_json::from_str::<AssetId>(&json).unwrap(), id);
        }
    }

    #[test]
    fn token_ids_are_stored_in_canonical_form() {
        let lower: AssetId = "bsc/0x55d398326f99059ff775485246999027b3197955"
            .parse()
            .unwrap();
        assert_eq!(
            lower.to_string(),
            "bsc/0x55d398326f99059fF775485246999027B3197955"
        );
        let friendly: AssetId = "ton/EQCxE6mUtQJKFnGfaROTKOt1lZbDiiX1kCixRv7Nw2Id_sDs"
            .parse()
            .unwrap();
        assert_eq!(
            friendly.contract().map(Address::family),
            Some(ChainFamily::Ton)
        );
        assert!(friendly.to_string().starts_with("ton/0:"));
    }

    #[test]
    fn exposes_network_and_contract() {
        let id: AssetId = "tron/TR7NHqjeKQxGTCi8q8ZY4pL8otSzgjLj6t".parse().unwrap();
        assert_eq!(id.network().as_str(), "tron");
        assert_eq!(id.contract().map(Address::family), Some(ChainFamily::Tron));
        let native: AssetId = "ton".parse().unwrap();
        assert_eq!(native.contract(), None);
    }

    #[test]
    fn rejects_broken_ids() {
        for text in [
            "",
            "BSC",
            "bsc/",
            "bsc/xyz",
            "bsc/0x123",
            "tron/TR7NHqjeKQxGTCi8q8ZY4pL8otSzgjLj6u",
        ] {
            assert!(text.parse::<AssetId>().is_err(), "{text:?}");
        }
        assert!(serde_json::from_str::<AssetId>("\"bsc/0xnothex\"").is_err());
    }
}
