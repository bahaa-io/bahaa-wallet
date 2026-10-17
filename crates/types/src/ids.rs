use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use thiserror::Error;

const MAX_ID_LEN: usize = 32;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
pub enum IdError {
    #[error("unknown chain family, expected evm, tron or ton")]
    ChainFamily,
    #[error(
        "network id must be lowercase letters, digits and single dashes, starting with a letter, at most {MAX_ID_LEN} long"
    )]
    NetworkId,
    #[error("price id must look like category/SYMBOL, for example crypto/BNB")]
    PriceId,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChainFamily {
    Evm,
    Tron,
    Ton,
}

impl ChainFamily {
    pub const ALL: [Self; 3] = [Self::Evm, Self::Tron, Self::Ton];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Evm => "evm",
            Self::Tron => "tron",
            Self::Ton => "ton",
        }
    }
}

impl fmt::Display for ChainFamily {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for ChainFamily {
    type Err = IdError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|family| family.as_str() == text)
            .ok_or(IdError::ChainFamily)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct NetworkId(String);

impl NetworkId {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for NetworkId {
    type Error = IdError;

    fn try_from(text: String) -> Result<Self, Self::Error> {
        let valid = (1..=MAX_ID_LEN).contains(&text.len())
            && text.starts_with(|c: char| c.is_ascii_lowercase())
            && !text.ends_with('-')
            && !text.contains("--")
            && text
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-');
        if valid {
            Ok(Self(text))
        } else {
            Err(IdError::NetworkId)
        }
    }
}

impl FromStr for NetworkId {
    type Err = IdError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        Self::try_from(text.to_owned())
    }
}

impl From<NetworkId> for String {
    fn from(id: NetworkId) -> Self {
        id.0
    }
}

impl fmt::Display for NetworkId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct PriceId {
    category: String,
    symbol: String,
}

impl PriceId {
    pub fn category(&self) -> &str {
        &self.category
    }

    pub fn symbol(&self) -> &str {
        &self.symbol
    }
}

impl TryFrom<String> for PriceId {
    type Error = IdError;

    fn try_from(text: String) -> Result<Self, Self::Error> {
        let (category, symbol) = text.split_once('/').ok_or(IdError::PriceId)?;
        let category_ok = (1..=MAX_ID_LEN).contains(&category.len())
            && category.starts_with(|c: char| c.is_ascii_lowercase())
            && category
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte == b'_');
        let symbol_ok = (1..=MAX_ID_LEN).contains(&symbol.len())
            && symbol
                .bytes()
                .any(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit())
            && symbol
                .bytes()
                .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_');
        if category_ok && symbol_ok {
            Ok(Self {
                category: category.to_owned(),
                symbol: symbol.to_owned(),
            })
        } else {
            Err(IdError::PriceId)
        }
    }
}

impl FromStr for PriceId {
    type Err = IdError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        Self::try_from(text.to_owned())
    }
}

impl From<PriceId> for String {
    fn from(id: PriceId) -> Self {
        id.to_string()
    }
}

impl fmt::Display for PriceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.category, self.symbol)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chain_families_round_trip() {
        for family in ChainFamily::ALL {
            assert_eq!(family.as_str().parse::<ChainFamily>(), Ok(family));
            let json = serde_json::to_string(&family).unwrap();
            assert_eq!(json, format!("\"{family}\""));
            assert_eq!(serde_json::from_str::<ChainFamily>(&json).unwrap(), family);
        }
        assert_eq!("EVM".parse::<ChainFamily>(), Err(IdError::ChainFamily));
        assert!(serde_json::from_str::<ChainFamily>("\"solana\"").is_err());
    }

    #[test]
    fn accepts_plain_network_ids() {
        for text in [
            "bsc",
            "ethereum",
            "bsc-testnet",
            "tron-nile",
            "ton-testnet",
            "op2",
            "a",
        ] {
            assert_eq!(text.parse::<NetworkId>().unwrap().as_str(), text);
        }
    }

    #[test]
    fn rejects_network_ids_that_could_confuse() {
        let long = "a".repeat(33);
        for text in [
            "",
            "BSC",
            "bsc_testnet",
            "-bsc",
            "bsc-",
            "bsc--testnet",
            "1inch",
            "bsc testnet",
            "bsc/x",
            "bѕc",
            &long,
        ] {
            assert_eq!(
                text.parse::<NetworkId>(),
                Err(IdError::NetworkId),
                "{text:?}"
            );
        }
    }

    #[test]
    fn network_ids_are_validated_when_read_from_config() {
        let id: NetworkId = serde_json::from_str("\"bsc\"").unwrap();
        assert_eq!(serde_json::to_string(&id).unwrap(), "\"bsc\"");
        assert!(serde_json::from_str::<NetworkId>("\"Bsc\"").is_err());
    }

    #[test]
    fn price_ids_match_the_feed_format() {
        for text in [
            "crypto/BNB",
            "crypto/POLYG",
            "crypto/1INCH",
            "iran_market/GOLD_18K",
            "forex/EURJPY",
        ] {
            let id: PriceId = text.parse().unwrap();
            assert_eq!(id.to_string(), text);
        }
        let id: PriceId = "crypto/GRAM".parse().unwrap();
        assert_eq!((id.category(), id.symbol()), ("crypto", "GRAM"));
    }

    #[test]
    fn rejects_malformed_price_ids() {
        for text in [
            "BNB",
            "crypto/",
            "/BNB",
            "Crypto/BNB",
            "crypto/bnb",
            "crypto/B-NB",
            "crypto/BNB/X",
            "crypto/___",
            "crypto /BNB",
        ] {
            assert_eq!(text.parse::<PriceId>(), Err(IdError::PriceId), "{text:?}");
        }
        assert!(serde_json::from_str::<PriceId>("\"crypto/bnb\"").is_err());
    }
}
