use std::fmt;

use bahaa_types::{Address, Amount, AssetId, is_plain_text};
use thiserror::Error;

use crate::PoolError;

const MAX_TX_ID: usize = 128;

#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum ChainError {
    #[error("network problem: {0}")]
    Network(String),
    #[error("the network refused the transaction: {0}")]
    Rejected(String),
    #[error("not enough {symbol}: this needs {needed}, the balance is {available}")]
    Insufficient {
        symbol: String,
        needed: Amount,
        available: Amount,
    },
    #[error("{0}")]
    Invalid(&'static str),
    #[error("this network does not support {0}")]
    Unsupported(&'static str),
    #[error("the signed transaction does not match the request: {0} differs")]
    Mismatch(&'static str),
    #[error(transparent)]
    Key(#[from] bahaa_crypto::DeriveError),
}

impl From<PoolError<ChainError>> for ChainError {
    fn from(err: PoolError<ChainError>) -> Self {
        match err {
            PoolError::Fatal(err) => err,
            PoolError::Exhausted(err) | PoolError::Unserved(err) => Self::Network(err.to_string()),
            PoolError::Unavailable(_) | PoolError::NoEndpoint => Self::Network(err.to_string()),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TxId(String);

impl TxId {
    pub fn new(text: impl Into<String>) -> Result<Self, ChainError> {
        let text = text.into();
        let allowed = text
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"+/=_-:".contains(&byte));
        if allowed && is_plain_text(&text, MAX_TX_ID) {
            Ok(Self(text))
        } else {
            Err(ChainError::Invalid("not a transaction id"))
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for TxId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Balance {
    pub asset: AssetId,
    pub amount: Amount,
    pub staked: Option<Amount>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Transfer {
    pub account: u32,
    pub from: Address,
    pub to: Address,
    pub asset: AssetId,
    pub amount: Amount,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FeeItem {
    Gas { units: u64, price: Amount },
    L1Data,
    Operator,
    Bandwidth { bytes: u64 },
    Energy { units: u64 },
    Activation,
    Compute { gas: u64 },
    Storage,
    Forwarding,
    Network,
    Contracts,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FeeLine {
    pub item: FeeItem,
    pub amount: Amount,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FeeQuote {
    max: Amount,
    expected: Amount,
    lines: Vec<FeeLine>,
}

impl FeeQuote {
    pub fn new(max: Amount, expected: Amount, lines: Vec<FeeLine>) -> Result<Self, ChainError> {
        let unbalanced = |_| ChainError::Invalid("fee lines do not add up");
        let mut total = Amount::zero(expected.decimals()).map_err(unbalanced)?;
        for line in &lines {
            total = total.checked_add(line.amount).map_err(unbalanced)?;
        }
        if total != expected {
            return Err(ChainError::Invalid(
                "fee lines must add up to the expected fee",
            ));
        }
        if expected
            .partial_cmp(&max)
            .is_none_or(std::cmp::Ordering::is_gt)
        {
            return Err(ChainError::Invalid(
                "the expected fee cannot exceed the maximum",
            ));
        }
        Ok(Self {
            max,
            expected,
            lines,
        })
    }

    pub fn max(&self) -> Amount {
        self.max
    }

    pub fn expected(&self) -> Amount {
        self.expected
    }

    pub fn lines(&self) -> &[FeeLine] {
        &self.lines
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Signed {
    pub id: TxId,
    pub bytes: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Decoded {
    pub from: Address,
    pub to: Address,
    pub asset: AssetId,
    pub amount: Amount,
    pub sequence: Option<u64>,
    pub expires_at: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TxStatus {
    Pending,
    Confirmed {
        block: Option<u64>,
        fee: Option<Amount>,
    },
    Failed(String),
    NotFound,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScanCursor {
    pub next: u64,
    pub complete_from: u64,
    pub complete_since: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Incoming {
    pub id: TxId,
    pub index: u64,
    pub from: Address,
    pub to: Address,
    pub asset: AssetId,
    pub amount: Amount,
    pub block: u64,
    pub time: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Outgoing {
    pub id: TxId,
    pub index: u64,
    pub from: Address,
    pub to: Address,
    pub asset: AssetId,
    pub amount: Amount,
    pub block: u64,
    pub time: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Scanned {
    pub cursor: ScanCursor,
    pub found: Vec<Incoming>,
    pub sent: Vec<Outgoing>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Incoming,
    Outgoing,
    ToSelf,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoryEntry {
    pub id: TxId,
    pub time: u64,
    pub direction: Direction,
    pub counterparty: Option<Address>,
    pub asset: AssetId,
    pub amount: Amount,
    pub fee: Option<Amount>,
    pub status: TxStatus,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cursor(pub String);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoryPage {
    pub entries: Vec<HistoryEntry>,
    pub next: Option<Cursor>,
}

pub fn check_signed(transfer: &Transfer, decoded: &Decoded) -> Result<(), ChainError> {
    let checks = [
        (decoded.from == transfer.from, "the sender"),
        (decoded.to == transfer.to, "the recipient"),
        (decoded.asset == transfer.asset, "the asset"),
        (decoded.amount == transfer.amount, "the amount"),
    ];
    match checks.into_iter().find(|(same, _)| !same) {
        Some((_, field)) => Err(ChainError::Mismatch(field)),
        None => Ok(()),
    }
}
