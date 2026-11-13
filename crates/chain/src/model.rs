use std::fmt;

use bahaa_types::{Address, Amount, AssetId, NetworkId, Nft, NftId, is_plain_text, to_plain_text};
use ruint::aliases::U256;
use thiserror::Error;

use crate::{Failure, PoolError};

const MAX_TX_ID: usize = 128;
pub const MAX_MEMO: usize = 120;

#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum ChainError {
    #[error("network problem: {0}")]
    Network(String),
    #[error("the public servers no longer keep this part of the history")]
    Pruned,
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
            PoolError::Exhausted(Self::Pruned) | PoolError::Unserved(Self::Pruned) => Self::Pruned,
            PoolError::Exhausted(Self::Network(text))
            | PoolError::Unserved(Self::Network(text)) => Self::Network(text),
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
    pub max: bool,
    pub memo: Option<String>,
}

impl Transfer {
    pub fn check(&self, network: &NetworkId) -> Result<(), ChainError> {
        if self.asset.network() != network {
            return Err(ChainError::Invalid("the asset belongs to another network"));
        }
        if self.asset.is_nft() {
            return Err(ChainError::Unsupported("sending NFTs"));
        }
        if !self.max && self.amount.is_zero() {
            return Err(ChainError::Invalid("send more than zero"));
        }
        Ok(())
    }

    pub fn check_nft(&self, network: &NetworkId) -> Result<&NftId, ChainError> {
        if self.asset.network() != network {
            return Err(ChainError::Invalid("the asset belongs to another network"));
        }
        let nft = self
            .asset
            .nft()
            .ok_or(ChainError::Invalid("this is not an NFT"))?;
        if !self.max && (self.amount.decimals() != 0 || self.amount.is_zero()) {
            return Err(ChainError::Invalid("send a whole number of NFTs"));
        }
        Ok(nft)
    }

    pub fn take_all(&mut self, held: Amount) -> Result<(), ChainError> {
        if held.is_zero() {
            return Err(ChainError::Invalid("there is nothing to send"));
        }
        self.amount = held;
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Call {
    pub to: Address,
    pub value: Amount,
    pub data: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContractSwap {
    pub account: u32,
    pub from: Address,
    pub call: Call,
    pub spender: Option<Address>,
    pub sell: AssetId,
    pub amount: Amount,
    pub buy: Option<(AssetId, Amount)>,
    pub ours: bool,
    pub spent: Option<Amount>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Approval {
    pub account: u32,
    pub from: Address,
    pub asset: AssetId,
    pub spender: Address,
    pub amount: Amount,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Step {
    Approve {
        asset: AssetId,
        spender: Address,
        amount: Amount,
    },
    Swap,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlannedCall {
    pub step: Step,
    pub account: u32,
    pub from: Address,
    pub call: Call,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DecodedCall {
    pub from: Address,
    pub call: Call,
    pub sequence: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Replace {
    SpeedUp,
    Cancel,
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
    Contracts,
    Memo,
    Signature,
    Priority { units: u64 },
    Size { vbytes: u64 },
    Leftover,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FeeLine {
    pub item: FeeItem,
    pub amount: Amount,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Wait {
    About(u64),
    Over(u64),
    Slow,
    Stuck,
}

impl Wait {
    pub fn blocks(count: u64, block_ms: u32) -> Self {
        Self::About(seconds(count, block_ms))
    }

    pub fn over_blocks(count: u64, block_ms: u32) -> Self {
        Self::Over(seconds(count, block_ms))
    }
}

fn seconds(blocks: u64, block_ms: u32) -> u64 {
    blocks.saturating_mul(u64::from(block_ms)).div_ceil(1_000)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RateUnit {
    SatPerVbyte,
    Gwei,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FeeRate {
    pub amount: Amount,
    pub unit: RateUnit,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Pace {
    pub rate: Option<FeeRate>,
    pub wait: Option<Wait>,
}

impl Pace {
    pub fn blocks(count: u64, block_ms: u32) -> Self {
        Self {
            rate: None,
            wait: Some(Wait::blocks(count, block_ms)),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Replaced {
    pub fee: Amount,
    pub pace: Pace,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FeeQuote {
    max: Amount,
    expected: Amount,
    lines: Vec<FeeLine>,
    pace: Pace,
    replaced: Option<Replaced>,
    withheld: Option<Amount>,
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
            pace: Pace::default(),
            replaced: None,
            withheld: None,
        })
    }

    #[must_use]
    pub fn paced(mut self, pace: Pace) -> Self {
        self.pace = pace;
        self
    }

    #[must_use]
    pub fn replacing(mut self, replaced: Replaced) -> Self {
        self.replaced = Some(replaced);
        self
    }

    #[must_use]
    pub fn withholding(mut self, withheld: Amount) -> Self {
        self.withheld = Some(withheld);
        self
    }

    pub fn pace(&self) -> Pace {
        self.pace
    }

    pub fn withheld(&self) -> Option<Amount> {
        self.withheld
    }

    pub fn replaced(&self) -> Option<Replaced> {
        self.replaced
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
    pub everything: bool,
    pub memo: Option<String>,
    pub sequence: Option<u64>,
    pub expires_at: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TxStatus {
    Pending,
    Confirmed {
        block: Option<u64>,
        fee: Option<Amount>,
        transaction: Option<TxId>,
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
pub struct Movement {
    pub id: TxId,
    pub index: u64,
    pub from: Address,
    pub to: Address,
    pub asset: AssetId,
    pub amount: Amount,
    pub block: u64,
    pub time: u64,
    pub memo: Option<String>,
}

pub type Incoming = Movement;
pub type Outgoing = Movement;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Watch {
    pub block: u64,
    pub balance: Amount,
    pub nonce: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Found {
    pub items: Vec<Nft>,
    pub capped: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Scanned {
    pub cursor: ScanCursor,
    pub found: Vec<Incoming>,
    pub sent: Vec<Outgoing>,
    pub watch: Option<Watch>,
}

impl ChainError {
    pub fn overflow() -> Self {
        Self::Invalid("fee overflow")
    }

    pub fn insufficient(symbol: String, needed: U256, available: U256, decimals: u8) -> Self {
        match (amount(needed, decimals), amount(available, decimals)) {
            (Ok(needed), Ok(available)) => Self::Insufficient {
                symbol,
                needed,
                available,
            },
            (Err(err), _) | (_, Err(err)) => err,
        }
    }

    pub fn malformed() -> Self {
        Self::Network("an endpoint sent a malformed reply".to_owned())
    }
}

impl Failure<ChainError> {
    pub fn retry(text: &str) -> Self {
        Self::Retry(ChainError::Network(text.to_owned()))
    }

    pub fn unsupported(text: &str) -> Self {
        Self::Unsupported(ChainError::Network(text.to_owned()))
    }

    pub fn unreachable(text: &str) -> Self {
        Self::Unreachable(ChainError::Network(text.to_owned()))
    }
}

pub fn enough(symbol: &str, needed: U256, held: U256, decimals: u8) -> Result<(), ChainError> {
    if held < needed {
        return Err(ChainError::insufficient(
            symbol.to_owned(),
            needed,
            held,
            decimals,
        ));
    }
    Ok(())
}

pub fn on_network(network: &NetworkId, assets: &[AssetId]) -> Result<(), ChainError> {
    if assets.iter().any(|asset| asset.network() != network) {
        return Err(ChainError::Invalid("an asset belongs to another network"));
    }
    if assets.iter().any(AssetId::is_nft) {
        return Err(ChainError::Invalid("NFTs are listed, not read as balances"));
    }
    Ok(())
}

pub fn amount(units: U256, decimals: u8) -> Result<Amount, ChainError> {
    Amount::new(units, decimals).map_err(|_| ChainError::Invalid("impossible decimals"))
}

pub fn memo_text(bytes: &[u8]) -> Option<String> {
    let text = to_plain_text(&String::from_utf8_lossy(bytes), MAX_MEMO);
    (!text.is_empty()).then_some(text)
}

pub fn check_call(planned: &PlannedCall, decoded: &DecodedCall) -> Result<(), ChainError> {
    let checks = [
        (decoded.from == planned.from, "the sender"),
        (decoded.call.to == planned.call.to, "the contract"),
        (decoded.call.value == planned.call.value, "the amount"),
        (decoded.call.data == planned.call.data, "the call"),
    ];
    match checks.into_iter().find(|(same, _)| !same) {
        Some((_, field)) => Err(ChainError::Mismatch(field)),
        None => Ok(()),
    }
}

pub fn check_signed(transfer: &Transfer, decoded: &Decoded) -> Result<(), ChainError> {
    let checks = [
        (decoded.from == transfer.from, "the sender"),
        (decoded.to == transfer.to, "the recipient"),
        (decoded.asset == transfer.asset, "the asset"),
        (decoded.amount == transfer.amount, "the amount"),
        (transfer.max || !decoded.everything, "the amount"),
        (decoded.memo == transfer.memo, "the memo"),
    ];
    match checks.into_iter().find(|(same, _)| !same) {
        Some((_, field)) => Err(ChainError::Mismatch(field)),
        None => Ok(()),
    }
}
