mod protocol;
mod rest;
mod service;

pub use protocol::{Quote, Quotes, condition, is_base, query};
pub use rest::{Fetch, MAX_REPLY, MarketError, RestClient};
pub use service::{Market, MarketConfig, Request, Snapshot, Status};
