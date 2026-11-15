use bahaa_types::{Address, NetworkId};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NameRecord {
    pub name: String,
    pub network: NetworkId,
    pub owner: Address,
    pub wallet: Option<Address>,
    pub elsewhere: bool,
    pub expires_at: Option<u64>,
    pub renewable: bool,
    pub reversible: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Link {
    To(Address),
    Clear,
    Renew,
    Primary,
}
