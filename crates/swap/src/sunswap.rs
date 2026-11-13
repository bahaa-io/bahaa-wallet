use bahaa_chain::{Call, Chain, ChainError, Wait};
use bahaa_tron::TronChain;
use bahaa_types::{Address, Amount, AssetId, ChainFamily, NetworkId};
use ruint::aliases::U256;

use crate::config::SunSwapConfig;
use crate::order::{Contract, Order, Provider, QUOTE_LIFE, Request, SwapError};
use crate::reply::unreadable;
use crate::route::{Charged, Cost, CostKind, Method, Route};
use bahaa_chain::abi;

const AMOUNTS_OUT: [u8; 4] = [0xd0, 0x6c, 0xa6, 0x1f];
const COINS_FOR_TOKENS: [u8; 4] = [0x7f, 0xf3, 0x6a, 0xb5];
const TOKENS_FOR_COINS: [u8; 4] = [0x18, 0xcb, 0xaf, 0xe5];
const TOKENS_FOR_TOKENS: [u8; 4] = [0x38, 0xed, 0x17, 0x39];
const SLIPPAGE_BPS: u32 = 50;
const WHOLE_BPS: u32 = 10_000;
const DEADLINE: u64 = 600;

pub struct SunSwap {
    network: NetworkId,
    router: [u8; 20],
    wrapped: [u8; 20],
    fee_bps: u32,
}

impl SunSwap {
    pub fn new(config: &SunSwapConfig) -> Result<Self, SwapError> {
        let tron = |text: &str| match Address::parse(ChainFamily::Tron, text) {
            Ok(Address::Tron(bytes)) => Ok(bytes),
            _ => Err(SwapError::Untrusted(
                "the built-in SunSwap router is broken",
            )),
        };
        Ok(Self {
            network: config.network.clone(),
            router: tron(&config.router)?,
            wrapped: tron(&config.wrapped)?,
            fee_bps: config.fee_bps,
        })
    }

    pub fn covers(&self, request: &Request) -> bool {
        *request.sell.network() == self.network
            && *request.buy.network() == self.network
            && request.sell != request.buy
            && matches!(
                (request.from, request.to),
                (Address::Tron(_), Address::Tron(_))
            )
    }

    pub async fn quote(
        &self,
        chain: &TronChain,
        request: &Request,
        now: u64,
    ) -> Result<(Route, Order), SwapError> {
        let Address::Tron(to) = request.to else {
            return Err(SwapError::Unsupported);
        };
        if !self.covers(request) {
            return Err(SwapError::Unsupported);
        }
        let path = [self.token(&request.sell)?, self.token(&request.buy)?];
        let units = request.amount.units();
        let mut asked = AMOUNTS_OUT.to_vec();
        abi::push_uint(&mut asked, units);
        abi::push_uint(&mut asked, U256::from(64u8));
        push_path(&mut asked, &path);
        let router = Address::Tron(self.router);
        let answer = match chain.read(&router, &asked).await {
            Ok(answer) => answer,
            Err(ChainError::Rejected(_)) => {
                return Err(SwapError::Refused("no pool trades these assets".to_owned()));
            }
            Err(err) => return Err(SwapError::Unreachable(err.to_string())),
        };
        let out = abi::uint(&answer, 3)
            .filter(|out| !out.is_zero() && abi::uint(&answer, 1) == Some(U256::from(2u8)))
            .ok_or_else(unreadable)?;
        let minimum = out
            .checked_mul(U256::from(WHOLE_BPS - SLIPPAGE_BPS))
            .map(|scaled| scaled / U256::from(WHOLE_BPS))
            .ok_or_else(unreadable)?;
        let data = swap_data(request, &path, &to, (units, minimum), now);
        let amount =
            |units: U256| Amount::new(units, request.buy_decimals).map_err(|_| unreadable());
        let (expected, minimum) = (amount(out)?, amount(minimum)?);
        let fee = units
            .checked_mul(U256::from(self.fee_bps))
            .map(|scaled| scaled / U256::from(WHOLE_BPS))
            .ok_or_else(unreadable)?;
        let route = Route {
            provider: Provider::SunSwap.name(),
            method: Method::Pool,
            sell: request.sell.clone(),
            amount: request.amount,
            buy: request.buy.clone(),
            expected,
            minimum,
            costs: vec![Cost {
                kind: CostKind::PoolFee,
                asset: request.sell.clone(),
                amount: Amount::new(fee, request.amount.decimals()).map_err(|_| unreadable())?,
                charged: Charged::InQuote,
            }],
            wait: Some(Wait::blocks(1, chain.network().block_ms)),
        };
        let native_in = request.sell.contract().is_none();
        let value = Amount::new(
            if native_in { units } else { U256::ZERO },
            request.native_decimals,
        )
        .map_err(|_| unreadable())?;
        let order = Order {
            provider: Provider::SunSwap,
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

    fn token(&self, asset: &AssetId) -> Result<[u8; 20], SwapError> {
        match asset.contract() {
            None => Ok(self.wrapped),
            Some(Address::Tron(bytes)) => Ok(*bytes),
            Some(_) => Err(SwapError::Unsupported),
        }
    }
}

fn swap_data(
    request: &Request,
    path: &[[u8; 20]; 2],
    to: &[u8; 20],
    (units, minimum): (U256, U256),
    now: u64,
) -> Vec<u8> {
    let deadline = U256::from(now.saturating_add(DEADLINE));
    let mut data;
    if request.sell.contract().is_none() {
        data = COINS_FOR_TOKENS.to_vec();
        abi::push_uint(&mut data, minimum);
        abi::push_uint(&mut data, U256::from(128u8));
    } else {
        data = if request.buy.contract().is_none() {
            TOKENS_FOR_COINS
        } else {
            TOKENS_FOR_TOKENS
        }
        .to_vec();
        abi::push_uint(&mut data, units);
        abi::push_uint(&mut data, minimum);
        abi::push_uint(&mut data, U256::from(160u8));
    }
    abi::push_address(&mut data, to);
    abi::push_uint(&mut data, deadline);
    push_path(&mut data, path);
    data
}

fn push_path(data: &mut Vec<u8>, path: &[[u8; 20]; 2]) {
    abi::push_uint(data, U256::from(path.len()));
    for token in path {
        abi::push_address(data, token);
    }
}
