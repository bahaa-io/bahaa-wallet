use std::fmt;
use std::str::FromStr;

use ruint::aliases::U256;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::address::{TON_FRIENDLY_LEN, TRON_LEN};
use crate::{Address, AddressError, ChainFamily, IdError, NetworkId};

#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub enum AssetId {
    Native(NetworkId),
    Token(NetworkId, Address),
    Nft(NetworkId, Box<NftId>),
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NftId {
    address: Address,
    token: Option<U256>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
pub enum AssetError {
    #[error(transparent)]
    Network(#[from] IdError),
    #[error(transparent)]
    Contract(#[from] AddressError),
    #[error("this is not an NFT id for that network")]
    Nft,
}

const NFT: &str = "nft/";
const MAX_TOKEN_DIGITS: usize = 78;

impl NftId {
    pub fn new(address: Address, token: Option<U256>) -> Result<Self, AssetError> {
        let numbered = match address.family() {
            ChainFamily::Evm | ChainFamily::Tron => true,
            ChainFamily::Ton | ChainFamily::Solana => false,
            ChainFamily::Bitcoin => return Err(AssetError::Nft),
        };
        if numbered != token.is_some() {
            return Err(AssetError::Nft);
        }
        Ok(Self { address, token })
    }

    pub fn address(&self) -> &Address {
        &self.address
    }

    pub fn token(&self) -> Option<U256> {
        self.token
    }
}

impl fmt::Display for NftId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.token {
            Some(token) => write!(f, "{}/{token}", self.address),
            None => write!(f, "{}", self.address),
        }
    }
}

impl AssetId {
    pub fn network(&self) -> &NetworkId {
        match self {
            Self::Native(network) | Self::Token(network, _) | Self::Nft(network, _) => network,
        }
    }

    pub fn contract(&self) -> Option<&Address> {
        match self {
            Self::Native(_) | Self::Nft(..) => None,
            Self::Token(_, contract) => Some(contract),
        }
    }

    pub fn nft(&self) -> Option<&NftId> {
        match self {
            Self::Native(_) | Self::Token(..) => None,
            Self::Nft(_, nft) => Some(nft),
        }
    }

    pub fn is_nft(&self) -> bool {
        matches!(self, Self::Nft(..))
    }
}

impl FromStr for AssetId {
    type Err = AssetError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let Some((network, rest)) = text.split_once('/') else {
            return Ok(Self::Native(text.parse()?));
        };
        let network = network.parse()?;
        let Some(rest) = rest.strip_prefix(NFT) else {
            return Ok(Self::Token(network, Address::parse(family_of(rest), rest)?));
        };
        let (address, token) = match rest.split_once('/') {
            Some((address, token)) if numbered(family_of(address)) => (address, Some(token)),
            _ => (rest, None),
        };
        let address = Address::parse(family_of(address), address)?;
        let token = token.map(token_number).transpose()?;
        Ok(Self::Nft(network, Box::new(NftId::new(address, token)?)))
    }
}

fn family_of(address: &str) -> ChainFamily {
    if address.starts_with("0x") {
        ChainFamily::Evm
    } else if address.contains(':') || address.len() == TON_FRIENDLY_LEN {
        ChainFamily::Ton
    } else if address.len() == TRON_LEN && address.starts_with('T') {
        ChainFamily::Tron
    } else {
        ChainFamily::Solana
    }
}

fn numbered(family: ChainFamily) -> bool {
    matches!(family, ChainFamily::Evm | ChainFamily::Tron)
}

fn token_number(text: &str) -> Result<U256, AssetError> {
    let canonical = !text.is_empty()
        && text.len() <= MAX_TOKEN_DIGITS
        && text.bytes().all(|byte| byte.is_ascii_digit())
        && (text == "0" || !text.starts_with('0'));
    if !canonical {
        return Err(AssetError::Nft);
    }
    U256::from_str_radix(text, 10).map_err(|_| AssetError::Nft)
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
            Self::Nft(network, nft) => write!(f, "{network}/{NFT}{nft}"),
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
            "solana/EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v",
            "solana/TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA",
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
        let mint: AssetId = "solana/TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"
            .parse()
            .unwrap();
        assert_eq!(
            mint.contract().map(Address::family),
            Some(ChainFamily::Solana)
        );
    }

    #[test]
    fn nft_ids_round_trip_in_canonical_form() {
        for text in [
            "ethereum/nft/0xBC4CA0EdA7647A8aB7C2061c2E118A18a936f13D/1",
            "ethereum/nft/0xBC4CA0EdA7647A8aB7C2061c2E118A18a936f13D/0",
            "avalanche/nft/0x0e11d8d829989085eED704295c4d3269746760eb/115792089237316195423570985008687907853269984665640564039457584007913129639935",
            "ton/nft/0:62f9d7e6635da5377c4b1386f5f82de306f3ac20819012ba3790a9db6e6eaa58",
            "solana/nft/EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v",
        ] {
            let id: AssetId = text.parse().unwrap();
            assert!(id.is_nft());
            assert_eq!(id.contract(), None);
            assert_eq!(id.to_string(), text);
            let json = serde_json::to_string(&id).unwrap();
            assert_eq!(serde_json::from_str::<AssetId>(&json).unwrap(), id);
        }
        let lower: AssetId = "ethereum/nft/0xbc4ca0eda7647a8ab7c2061c2e118a18a936f13d/7"
            .parse()
            .unwrap();
        assert_eq!(
            lower.to_string(),
            "ethereum/nft/0xBC4CA0EdA7647A8aB7C2061c2E118A18a936f13D/7"
        );
        let nft = lower.nft().unwrap();
        assert_eq!(nft.token(), Some(U256::from(7)));
        assert_eq!(nft.address().family(), ChainFamily::Evm);
        let friendly: AssetId = "ton/nft/EQCxE6mUtQJKFnGfaROTKOt1lZbDiiX1kCixRv7Nw2Id_sDs"
            .parse()
            .unwrap();
        assert!(friendly.to_string().starts_with("ton/nft/0:"));
    }

    #[test]
    fn nft_ids_need_a_token_number_exactly_where_the_network_numbers_items() {
        for text in [
            "ethereum/nft/0xBC4CA0EdA7647A8aB7C2061c2E118A18a936f13D",
            "ethereum/nft/0xBC4CA0EdA7647A8aB7C2061c2E118A18a936f13D/",
            "ethereum/nft/0xBC4CA0EdA7647A8aB7C2061c2E118A18a936f13D/01",
            "ethereum/nft/0xBC4CA0EdA7647A8aB7C2061c2E118A18a936f13D/+1",
            "ethereum/nft/0xBC4CA0EdA7647A8aB7C2061c2E118A18a936f13D/1.0",
            "ethereum/nft/0xBC4CA0EdA7647A8aB7C2061c2E118A18a936f13D/1_000",
            "ethereum/nft/0xBC4CA0EdA7647A8aB7C2061c2E118A18a936f13D/115792089237316195423570985008687907853269984665640564039457584007913129639936",
            "ethereum/nft/0xBC4CA0EdA7647A8aB7C2061c2E118A18a936f13D/1/2",
            "ton/nft/0:62f9d7e6635da5377c4b1386f5f82de306f3ac20819012ba3790a9db6e6eaa58/1",
            "solana/nft/EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v/1",
            "bitcoin/nft/bc1qar0srrr7xfkvy5l643lydnw9re59gtzzwf5mdq",
            "ethereum/nft",
            "ethereum/nft/",
        ] {
            assert!(text.parse::<AssetId>().is_err(), "{text:?}");
        }
        let bitcoin = Address::parse(
            ChainFamily::Bitcoin,
            "bc1qar0srrr7xfkvy5l643lydnw9re59gtzzwf5mdq",
        )
        .unwrap();
        assert_eq!(NftId::new(bitcoin, None), Err(AssetError::Nft));
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
            "solana/EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt0",
        ] {
            assert!(text.parse::<AssetId>().is_err(), "{text:?}");
        }
        assert!(serde_json::from_str::<AssetId>("\"bsc/0xnothex\"").is_err());
    }
}
