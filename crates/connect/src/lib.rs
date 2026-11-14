mod bridge;
#[cfg(fuzzing)]
#[doc(hidden)]
pub mod fuzz;
mod link;
mod manifest;
mod reply;
mod request;
mod session;
mod sign;
mod wallets;

use thiserror::Error;

pub use bridge::{Bridge, Incoming};
pub use link::{Invitation, read_link};
pub use manifest::{Manifest, fetch_manifest, read_manifest};
pub use reply::{
    Code, Device, address_item, answer, connected, disconnected, failure, proof_item, refused,
    signed_data, unsupported_item,
};
pub use request::{
    AppRequest, Bad, Call, ConnectItem, ConnectRequest, Content, JettonItem, MAX_MESSAGES, Message,
    NftItem, Outgoing, SignData, Transaction, TransferItem, read_request,
};
pub use session::SessionKey;
pub use sign::{data_hash, proof_hash, schema_crc};
pub use wallets::{Bridges, bridge_for, read_wallets};

#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum ConnectError {
    #[error("this isn't a TON Connect link")]
    NotLink,
    #[error("{0}")]
    Invalid(&'static str),
    #[error("{0}")]
    Unsupported(&'static str),
    #[error("the app's bridge can't be reached: {0}")]
    Network(String),
    #[error("the app's bridge refused the message (HTTP {0})")]
    Refused(u16),
    #[error("{0}")]
    Manifest(&'static str),
    #[error("a message from the app couldn't be opened")]
    Sealed,
}
