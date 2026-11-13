use std::collections::BTreeMap;

use bahaa_chain::{Call, Chain, ChainError, Wait};
use bahaa_evm::EvmChain;
use bahaa_types::{Address, Amount, AssetId, ChainFamily, NetworkId};
use futures_util::future::join_all;
use ruint::aliases::U256;

use crate::config::PoolConfig;
use crate::order::{Contract, Order, Provider, QUOTE_LIFE, Request, SwapError};
use crate::route::{Charged, Cost, CostKind, Method, Route};
use bahaa_chain::abi;

const QUOTE_EXACT_INPUT_SINGLE: [u8; 4] = [0xc6, 0xa5, 0x02, 0x6a];
const EXACT_INPUT_SINGLE: [u8; 4] = [0x04, 0xe4, 0x5a, 0xaf];
const UNWRAP: [u8; 4] = [0x49, 0x40, 0x4b, 0x7c];
const MULTICALL: [u8; 4] = [0x5a, 0xe4, 0x01, 0xdc];
const ROUTER_ITSELF: [u8; 20] = [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2];
const SLIPPAGE_BPS: u32 = 50;
const WHOLE_BPS: u32 = 10_000;
const FEE_UNITS: u32 = 1_000_000;
const DEADLINE: u64 = 600;

struct Venue {
    provider: Provider,
    quoter: Address,
    router: [u8; 20],
    wrapped: [u8; 20],
    fees: Vec<u32>,
}

pub struct Pools {
    venues: BTreeMap<NetworkId, Venue>,
}

impl Pools {
    pub fn new(config: &BTreeMap<NetworkId, PoolConfig>) -> Result<Self, SwapError> {
        let broken = || SwapError::Untrusted("the built-in pool list is broken");
        let evm = |text: &str| match Address::parse(ChainFamily::Evm, text) {
            Ok(Address::Evm(bytes)) => Ok(bytes),
            _ => Err(broken()),
        };
        let venues = config
            .iter()
            .map(|(network, venue)| {
                let dex = matches!(venue.provider, Provider::Uniswap | Provider::PancakeSwap);
                let fees_ok = !venue.fees.is_empty()
                    && venue.fees.iter().all(|fee| (1..FEE_UNITS).contains(fee));
                if !dex || !fees_ok {
                    return Err(broken());
                }
                Ok((
                    network.clone(),
                    Venue {
                        provider: venue.provider,
                        quoter: Address::Evm(evm(&venue.quoter)?),
                        router: evm(&venue.router)?,
                        wrapped: evm(&venue.wrapped)?,
                        fees: venue.fees.clone(),
                    },
                ))
            })
            .collect::<Result<_, _>>()?;
        Ok(Self { venues })
    }

    pub fn provider(&self, network: &NetworkId) -> Option<Provider> {
        self.venues.get(network).map(|venue| venue.provider)
    }

    pub fn covers(&self, request: &Request) -> bool {
        request.sell.network() == request.buy.network()
            && request.sell != request.buy
            && self.venues.contains_key(request.sell.network())
            && matches!(request.to, Address::Evm(_))
    }

    pub async fn quote(
        &self,
        chain: &EvmChain,
        request: &Request,
        now: u64,
    ) -> Result<(Route, Order), SwapError> {
        let venue = self
            .venues
            .get(request.sell.network())
            .filter(|_| self.covers(request))
            .ok_or(SwapError::Unsupported)?;
        let paid = token(venue, &request.sell)?;
        let bought = token(venue, &request.buy)?;
        let units = request.amount.units();
        let (fee, out) = best(chain, venue, &paid, &bought, units).await?;
        let expected = Amount::new(out, request.buy_decimals).map_err(|_| unreadable())?;
        let minimum = Amount::new(
            out.checked_mul(U256::from(WHOLE_BPS - SLIPPAGE_BPS))
                .map(|scaled| scaled / U256::from(WHOLE_BPS))
                .ok_or_else(unreadable)?,
            request.buy_decimals,
        )
        .map_err(|_| unreadable())?;
        let Address::Evm(recipient) = request.to else {
            return Err(SwapError::Unsupported);
        };
        let native_out = request.buy.contract().is_none();
        let single = swap_data(
            &paid,
            &bought,
            fee,
            if native_out {
                &ROUTER_ITSELF
            } else {
                &recipient
            },
            units,
            minimum.units(),
        );
        let mut steps = vec![single];
        if native_out {
            steps.push(unwrap_data(minimum.units(), &recipient));
        }
        let data = multicall_data(now.saturating_add(DEADLINE), &steps);
        let native_in = request.sell.contract().is_none();
        let decimals = chain.network().native.decimals;
        let value = Amount::new(if native_in { units } else { U256::ZERO }, decimals)
            .map_err(|_| unreadable())?;
        let pool_fee = units
            .checked_mul(U256::from(fee))
            .map(|scaled| scaled / U256::from(FEE_UNITS))
            .ok_or_else(unreadable)?;
        let route = Route {
            provider: venue.provider.name(),
            method: Method::Pool,
            sell: request.sell.clone(),
            amount: request.amount,
            buy: request.buy.clone(),
            expected,
            minimum,
            costs: vec![Cost {
                kind: CostKind::PoolFee,
                asset: request.sell.clone(),
                amount: Amount::new(pool_fee, request.amount.decimals())
                    .map_err(|_| unreadable())?,
                charged: Charged::InQuote,
            }],
            wait: Some(Wait::blocks(1, chain.network().block_ms)),
        };
        let router = Address::Evm(venue.router);
        let order = Order {
            provider: venue.provider,
            reference: String::new(),
            deposit: router,
            comment: None,
            amount: request.amount,
            expected,
            minimum,
            expires_at: now.saturating_add(QUOTE_LIFE),
            contract: Some(Contract {
                call: Call {
                    to: router,
                    value,
                    data,
                },
                spender: (!native_in).then_some(router),
                ours: true,
                spent: None,
            }),
            typed: None,
        };
        Ok((route, order))
    }
}

async fn best(
    chain: &EvmChain,
    venue: &Venue,
    paid: &[u8; 20],
    bought: &[u8; 20],
    units: U256,
) -> Result<(u32, U256), SwapError> {
    let asks = venue.fees.iter().map(|fee| async move {
        let data = quote_data(paid, bought, units, *fee);
        (*fee, chain.read(&venue.quoter, &data).await)
    });
    let mut best: Option<(u32, U256)> = None;
    let mut failure = None;
    for (fee, answer) in join_all(asks).await {
        match answer {
            Ok(output) => {
                let out = abi::uint(&output, 0).ok_or_else(unreadable)?;
                if !out.is_zero() && best.is_none_or(|(_, seen)| out > seen) {
                    best = Some((fee, out));
                }
            }
            Err(ChainError::Rejected(_)) => {}
            Err(err) => failure = Some(err),
        }
    }
    best.ok_or_else(|| match failure {
        Some(err) => SwapError::Unreachable(err.to_string()),
        None => SwapError::Refused("no pool trades these assets".to_owned()),
    })
}

fn unreadable() -> SwapError {
    SwapError::Untrusted("the pool sent a quote this wallet can't read")
}

fn token(venue: &Venue, asset: &AssetId) -> Result<[u8; 20], SwapError> {
    match asset.contract() {
        None => Ok(venue.wrapped),
        Some(Address::Evm(bytes)) => Ok(*bytes),
        Some(_) => Err(SwapError::Unsupported),
    }
}

fn quote_data(sell: &[u8; 20], buy: &[u8; 20], units: U256, fee: u32) -> Vec<u8> {
    let mut data = QUOTE_EXACT_INPUT_SINGLE.to_vec();
    abi::push_address(&mut data, sell);
    abi::push_address(&mut data, buy);
    abi::push_uint(&mut data, units);
    abi::push_uint(&mut data, U256::from(fee));
    abi::push_uint(&mut data, U256::ZERO);
    data
}

fn swap_data(
    sell: &[u8; 20],
    buy: &[u8; 20],
    fee: u32,
    recipient: &[u8; 20],
    units: U256,
    minimum: U256,
) -> Vec<u8> {
    let mut data = EXACT_INPUT_SINGLE.to_vec();
    abi::push_address(&mut data, sell);
    abi::push_address(&mut data, buy);
    abi::push_uint(&mut data, U256::from(fee));
    abi::push_address(&mut data, recipient);
    abi::push_uint(&mut data, units);
    abi::push_uint(&mut data, minimum);
    abi::push_uint(&mut data, U256::ZERO);
    data
}

fn unwrap_data(minimum: U256, recipient: &[u8; 20]) -> Vec<u8> {
    let mut data = UNWRAP.to_vec();
    abi::push_uint(&mut data, minimum);
    abi::push_address(&mut data, recipient);
    data
}

fn multicall_data(deadline: u64, steps: &[Vec<u8>]) -> Vec<u8> {
    let mut data = MULTICALL.to_vec();
    abi::push_uint(&mut data, U256::from(deadline));
    abi::push_uint(&mut data, U256::from(64u8));
    abi::push_bytes_list(&mut data, steps);
    data
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_multicall_carries_the_deadline_and_every_step_in_order() {
        let data = multicall_data(7, &[vec![1, 2, 3], vec![4; 33]]);
        assert_eq!(data[..4], MULTICALL);
        assert_eq!(abi::uint(&data[4..], 0), Some(U256::from(7u8)));
        assert_eq!(abi::uint(&data[4..], 1), Some(U256::from(64u8)));
        assert_eq!(abi::uint(&data[4..], 2), Some(U256::from(2u8)));
        assert_eq!(abi::uint(&data[4..], 3), Some(U256::from(64u8)));
        assert_eq!(abi::uint(&data[4..], 4), Some(U256::from(128u8)));
        assert_eq!(abi::uint(&data[4..], 5), Some(U256::from(3u8)));
        assert_eq!(data[4 + 6 * 32..4 + 6 * 32 + 3], [1, 2, 3]);
        assert_eq!(abi::uint(&data[4..], 7), Some(U256::from(33u8)));
        assert_eq!(data.len(), 4 + 10 * 32);
    }
}
