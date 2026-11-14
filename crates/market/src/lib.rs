#[cfg(fuzzing)]
#[doc(hidden)]
pub mod fuzz;
mod history;
mod protocol;
mod rest;
mod service;

pub use history::{MAX_POINTS, Point, Span};
pub use protocol::{Quote, Quotes};
pub use rest::{Fetch, MAX_REPLY, MarketError, RestClient};
pub use service::{Market, MarketConfig, Request, Snapshot, Status};
