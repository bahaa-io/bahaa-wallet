use bahaa_types::{Address, Amount, AssetId, TonForm};
use thiserror::Error;

pub const QUOTE_LIFE: u64 = 30;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Provider {
    OneClick,
    Relay,
}

impl Provider {
    pub fn name(self) -> &'static str {
        match self {
            Self::OneClick => "NEAR Intents",
            Self::Relay => "Relay",
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            Self::OneClick => "oneclick",
            Self::Relay => "relay",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        [Self::OneClick, Self::Relay]
            .into_iter()
            .find(|provider| provider.key() == key)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    pub sell: AssetId,
    pub amount: Amount,
    pub buy: AssetId,
    pub buy_decimals: u8,
    pub from: Address,
    pub to: Address,
    pub deadline: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Comment {
    Text(String),
    TonBody(Vec<u8>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Order {
    pub provider: Provider,
    pub reference: String,
    pub deposit: Address,
    pub comment: Option<Comment>,
    pub amount: Amount,
    pub expected: Amount,
    pub minimum: Amount,
    pub expires_at: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Progress {
    Waiting,
    Received,
    Swapping,
    Done,
    Refunded,
    Failed(String),
}

#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum SwapError {
    #[error("{0}")]
    Unreachable(String),
    #[error("{0}")]
    Refused(String),
    #[error("{0}")]
    Untrusted(&'static str),
    #[error("this provider does not swap these assets")]
    Unsupported,
}

pub(crate) fn text_of(address: &Address, bounceable: bool) -> String {
    address
        .ton_friendly(TonForm {
            bounceable,
            testnet: false,
        })
        .unwrap_or_else(|| address.to_string())
}

pub(crate) fn parse_like(text: &str, like: &Address) -> Result<Address, SwapError> {
    let address = Address::parse(like.family(), text).map_err(|_| {
        SwapError::Untrusted("the provider gave a deposit address this wallet can't read")
    })?;
    let same = match (&address, like) {
        (Address::Bitcoin { testnet, .. }, Address::Bitcoin { testnet: ours, .. }) => {
            testnet == ours
        }
        _ => address.family() == like.family(),
    };
    if !same || &address == like {
        return Err(SwapError::Untrusted(
            "the provider gave a deposit address on another network",
        ));
    }
    Ok(address)
}
