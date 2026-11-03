use bahaa_chain::Wait;
use bahaa_types::{Amount, AssetId};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Custody {
    Nobody,
    Escrow,
    Provider,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Method {
    Pool,
    Aggregator,
    Auction,
    Escrow,
    Deposit,
    Validators,
    Issuer,
}

impl Method {
    pub fn custody(self) -> Custody {
        match self {
            Self::Pool | Self::Aggregator | Self::Auction => Custody::Nobody,
            Self::Escrow | Self::Issuer => Custody::Escrow,
            Self::Deposit | Self::Validators => Custody::Provider,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CostKind {
    NetworkFee,
    ProviderFee,
    PoolFee,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Charged {
    InQuote,
    Extra,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cost {
    pub kind: CostKind,
    pub asset: AssetId,
    pub amount: Amount,
    pub charged: Charged,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Route {
    pub provider: &'static str,
    pub method: Method,
    pub sell: AssetId,
    pub amount: Amount,
    pub buy: AssetId,
    pub expected: Amount,
    pub minimum: Amount,
    pub costs: Vec<Cost>,
    pub wait: Option<Wait>,
}

impl Route {
    pub fn custody(&self) -> Custody {
        self.method.custody()
    }
}
