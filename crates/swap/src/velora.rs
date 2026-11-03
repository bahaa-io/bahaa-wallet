use std::collections::BTreeMap;

use bahaa_chain::{Call, Chain, Wait};
use bahaa_evm::EvmChain;
use bahaa_types::{Address, Amount, AssetId, ChainFamily, NetworkId};
use reqwest::Client;
use ruint::aliases::U256;
use serde_json::{Value, json};
use url::Url;

use crate::abi;
use crate::config::VeloraConfig;
use crate::order::{Contract, Order, Provider, QUOTE_LIFE, Request, SwapError};
use crate::reply::{fetch, unreadable};
use crate::route::{Charged, Cost, CostKind, Method, Route};

const SWAP: [u8; 4] = [0xe3, 0xea, 0xd5, 0x9e];
const NATIVE: [u8; 20] = [0xee; 20];
const METHOD: &str = "swapExactAmountIn";
const SLIPPAGE_BPS: u32 = 50;
const WHOLE_BPS: u32 = 10_000;
const FEE_BITS: u32 = 14;
const MAX_FEE_BPS: u32 = 100;

pub struct Velora {
    client: Client,
    base: Url,
    augustus: [u8; 20],
    chains: BTreeMap<NetworkId, u64>,
}

#[derive(Debug, PartialEq, Eq)]
struct Described {
    sold: [u8; 20],
    bought: [u8; 20],
    amount: U256,
    minimum: U256,
    quoted: U256,
    receiver: [u8; 20],
    fee_bps: u32,
}

impl Velora {
    pub fn new(config: &VeloraConfig, client: Client) -> Result<Self, SwapError> {
        let Ok(Address::Evm(augustus)) = Address::parse(ChainFamily::Evm, &config.augustus) else {
            return Err(SwapError::Untrusted(
                "the built-in Velora contract is broken",
            ));
        };
        Ok(Self {
            client,
            base: config.url.clone(),
            augustus,
            chains: config.chains.clone(),
        })
    }

    pub fn covers(&self, request: &Request) -> bool {
        request.sell.network() == request.buy.network()
            && request.sell != request.buy
            && self.chains.contains_key(request.sell.network())
            && matches!(
                (request.from, request.to),
                (Address::Evm(_), Address::Evm(_))
            )
    }

    pub async fn quote(
        &self,
        chain: &EvmChain,
        request: &Request,
        now: u64,
    ) -> Result<(Route, Order), SwapError> {
        let network = *self
            .chains
            .get(request.sell.network())
            .filter(|_| self.covers(request))
            .ok_or(SwapError::Unsupported)?;
        let (Address::Evm(from), Address::Evm(to)) = (request.from, request.to) else {
            return Err(SwapError::Unsupported);
        };
        let (sold, bought) = (token(&request.sell)?, token(&request.buy)?);
        let units = request.amount.units();
        let decimals = (
            request.amount.decimals().to_string(),
            request.buy_decimals.to_string(),
        );
        let mut price_url = self.base.join("prices").map_err(|_| unreadable())?;
        price_url
            .query_pairs_mut()
            .append_pair("srcToken", &hex(&sold))
            .append_pair("destToken", &hex(&bought))
            .append_pair("amount", &units.to_string())
            .append_pair("srcDecimals", &decimals.0)
            .append_pair("destDecimals", &decimals.1)
            .append_pair("side", "SELL")
            .append_pair("network", &network.to_string())
            .append_pair("version", "6.2")
            .append_pair("includeContractMethods", METHOD)
            .append_pair("userAddress", &hex(&from));
        let priced = fetch(self.client.get(price_url)).await?;
        let route = priced["priceRoute"].clone();
        if route["contractMethod"].as_str() != Some(METHOD) {
            return Err(SwapError::Untrusted(
                "the route uses another contract method",
            ));
        }
        let build_url = self
            .base
            .join(&format!("transactions/{network}?ignoreChecks=true"))
            .map_err(|_| unreadable())?;
        let built = fetch(self.client.post(build_url).json(&json!({
            "srcToken": hex(&sold),
            "destToken": hex(&bought),
            "srcAmount": units.to_string(),
            "srcDecimals": request.amount.decimals(),
            "destDecimals": request.buy_decimals,
            "slippage": SLIPPAGE_BPS,
            "priceRoute": route,
            "userAddress": hex(&from),
            "receiver": hex(&to),
        })))
        .await?;
        let quoted: U256 = route["destAmount"]
            .as_str()
            .and_then(|text| text.parse().ok())
            .ok_or_else(unreadable)?;
        let (described, data) = self.checked(&built, request, (sold, bought, to), quoted)?;
        self.offer(chain, request, &described, data, now)
    }

    fn checked(
        &self,
        built: &Value,
        request: &Request,
        (sold, bought, to): ([u8; 20], [u8; 20], [u8; 20]),
        quoted: U256,
    ) -> Result<(Described, Vec<u8>), SwapError> {
        let target = built["to"].as_str().ok_or_else(unreadable)?;
        if Address::parse(ChainFamily::Evm, target).map_err(|_| unreadable())?
            != Address::Evm(self.augustus)
        {
            return Err(SwapError::Untrusted("the route calls an unknown contract"));
        }
        let units = request.amount.units();
        let value: U256 = built["value"]
            .as_str()
            .and_then(|text| text.parse().ok())
            .ok_or_else(unreadable)?;
        let native_in = request.sell.contract().is_none();
        if value != if native_in { units } else { U256::ZERO } {
            return Err(SwapError::Untrusted("the route is for another amount"));
        }
        let data = built_data(built)?;
        let described = described(&data).ok_or(SwapError::Untrusted(
            "the route asks for more than a plain swap",
        ))?;
        let floor = quoted
            .checked_mul(U256::from(WHOLE_BPS - SLIPPAGE_BPS))
            .map(|scaled| scaled / U256::from(WHOLE_BPS))
            .ok_or_else(unreadable)?;
        let honest = described.sold == sold
            && described.bought == bought
            && described.receiver == to
            && described.amount == units
            && described.quoted == quoted
            && described.minimum >= floor
            && described.minimum <= quoted
            && described.fee_bps <= MAX_FEE_BPS;
        if !honest {
            return Err(SwapError::Untrusted(
                "the route pays someone else or less than it shows",
            ));
        }
        Ok((described, data))
    }

    fn offer(
        &self,
        chain: &EvmChain,
        request: &Request,
        described: &Described,
        data: Vec<u8>,
        now: u64,
    ) -> Result<(Route, Order), SwapError> {
        let after = |units: U256| {
            units
                .checked_mul(U256::from(WHOLE_BPS - described.fee_bps))
                .map(|scaled| scaled / U256::from(WHOLE_BPS))
                .ok_or_else(unreadable)
        };
        let received = after(described.quoted)?;
        let amount =
            |units: U256| Amount::new(units, request.buy_decimals).map_err(|_| unreadable());
        let (expected, minimum) = (amount(received)?, amount(after(described.minimum)?)?);
        let fee = described
            .quoted
            .checked_sub(received)
            .ok_or_else(unreadable)?;
        let costs = if fee.is_zero() {
            Vec::new()
        } else {
            vec![Cost {
                kind: CostKind::ProviderFee,
                asset: request.buy.clone(),
                amount: amount(fee)?,
                charged: Charged::InQuote,
            }]
        };
        let route = Route {
            provider: Provider::Velora.name(),
            method: Method::Aggregator,
            sell: request.sell.clone(),
            amount: request.amount,
            buy: request.buy.clone(),
            expected,
            minimum,
            costs,
            wait: Some(Wait::blocks(1, chain.network().block_ms)),
        };
        let augustus = Address::Evm(self.augustus);
        let native_in = request.sell.contract().is_none();
        let value = Amount::new(
            if native_in {
                request.amount.units()
            } else {
                U256::ZERO
            },
            chain.network().native.decimals,
        )
        .map_err(|_| unreadable())?;
        let order = Order {
            provider: Provider::Velora,
            reference: String::new(),
            deposit: augustus,
            comment: None,
            amount: request.amount,
            expected,
            minimum,
            expires_at: now.saturating_add(QUOTE_LIFE),
            contract: Some(Contract {
                call: Call {
                    to: augustus,
                    value,
                    data,
                },
                spender: (!native_in).then_some(augustus),
                ours: false,
            }),
            typed: None,
        };
        Ok((route, order))
    }
}

fn built_data(built: &Value) -> Result<Vec<u8>, SwapError> {
    built["data"]
        .as_str()
        .and_then(|text| text.strip_prefix("0x"))
        .and_then(bahaa_types::hex::decode)
        .ok_or_else(unreadable)
}

fn token(asset: &AssetId) -> Result<[u8; 20], SwapError> {
    match asset.contract() {
        None => Ok(NATIVE),
        Some(Address::Evm(bytes)) => Ok(*bytes),
        Some(_) => Err(SwapError::Unsupported),
    }
}

fn hex(bytes: &[u8; 20]) -> String {
    format!("0x{}", bahaa_types::hex::encode(bytes))
}

fn described(data: &[u8]) -> Option<Described> {
    let args = data.strip_prefix(&SWAP[..])?;
    let packed = abi::uint(args, 8)?;
    let fee_bps = u32::try_from(packed & U256::from((1u32 << FEE_BITS) - 1)).ok()?;
    (abi::length(args, 9)? == 0).then_some(())?;
    Some(Described {
        sold: abi::address(args, 1)?,
        bought: abi::address(args, 2)?,
        amount: abi::uint(args, 3)?,
        minimum: abi::uint(args, 4)?,
        quoted: abi::uint(args, 5)?,
        receiver: abi::address(args, 7)?,
        fee_bps,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> Vec<u8> {
        let built: Value =
            serde_json::from_str(include_str!("../tests/fixtures/velora-build.json")).unwrap();
        built_data(&built).unwrap()
    }

    #[test]
    fn a_real_velora_call_shows_its_receiver_amounts_and_fee() {
        let described = described(&fixture()).unwrap();
        assert_eq!(described.sold, NATIVE);
        assert_eq!(
            hex(&described.bought),
            "0x55d398326f99059ff775485246999027b3197955"
        );
        assert_eq!(
            hex(&described.receiver),
            "0x9858effd232b4033e47d90003d41ec34ecaeda94"
        );
        assert_eq!(described.amount, U256::from(100_000_000_000_000_000u64));
        assert_eq!(described.fee_bps, 1);
        assert!(described.minimum < described.quoted);
    }

    #[test]
    fn another_method_or_a_permit_is_not_read() {
        let mut other = fixture();
        other[1] = 0;
        assert_eq!(described(&other), None);
        assert_eq!(described(&fixture()[..100]), None);
    }
}
