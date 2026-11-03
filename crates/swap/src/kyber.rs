use std::collections::BTreeMap;

use bahaa_chain::{Call, Chain, Wait};
use bahaa_evm::EvmChain;
use bahaa_types::{Address, Amount, AssetId, ChainFamily, NetworkId};
use reqwest::Client;
use ruint::aliases::U256;
use serde_json::json;
use url::Url;

use crate::abi;
use crate::config::KyberConfig;
use crate::order::{Contract, Order, Provider, QUOTE_LIFE, Request, SwapError};
use crate::reply::{fetch, unreadable};
use crate::route::{Method, Route};

const SWAP: [u8; 4] = [0xe2, 0x1f, 0xd0, 0xe9];
const NATIVE: [u8; 20] = [0xee; 20];
const PARTIAL_FILL: u8 = 1;
const SLIPPAGE_BPS: u32 = 50;
const WHOLE_BPS: u32 = 10_000;
const DEADLINE: u64 = 600;

pub struct KyberSwap {
    client: Client,
    base: Url,
    router: [u8; 20],
    chains: BTreeMap<NetworkId, String>,
}

#[derive(Debug, PartialEq, Eq)]
struct Described {
    sold: [u8; 20],
    bought: [u8; 20],
    receiver: [u8; 20],
    amount: U256,
    minimum: U256,
}

impl KyberSwap {
    pub fn new(config: &KyberConfig, client: Client) -> Result<Self, SwapError> {
        let Ok(Address::Evm(router)) = Address::parse(ChainFamily::Evm, &config.router) else {
            return Err(SwapError::Untrusted(
                "the built-in KyberSwap router is broken",
            ));
        };
        Ok(Self {
            client,
            base: config.url.clone(),
            router,
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
        let name = self
            .chains
            .get(request.sell.network())
            .filter(|_| self.covers(request))
            .ok_or(SwapError::Unsupported)?;
        let (Address::Evm(from), Address::Evm(to)) = (request.from, request.to) else {
            return Err(SwapError::Unsupported);
        };
        let (sold, bought) = (token(&request.sell)?, token(&request.buy)?);
        let units = request.amount.units();
        let summary = self.summary(name, &sold, &bought, units).await?;
        let build_url = self.url(&format!("{name}/api/v1/route/build"))?;
        let built = fetch(self.client.post(build_url).json(&json!({
            "routeSummary": summary,
            "sender": hex(&from),
            "recipient": hex(&to),
            "slippageTolerance": SLIPPAGE_BPS,
            "deadline": now.saturating_add(DEADLINE),
        })))
        .await?;
        let data = &built["data"];
        let text = |field: &str| data[field].as_str().ok_or_else(unreadable);
        let called =
            Address::parse(ChainFamily::Evm, text("routerAddress")?).map_err(|_| unreadable())?;
        if called != Address::Evm(self.router) {
            return Err(SwapError::Untrusted("the route calls an unknown contract"));
        }
        let number = |field: &str| text(field)?.parse::<U256>().map_err(|_| unreadable());
        let out = number("amountOut")?;
        let native_in = request.sell.contract().is_none();
        let value = if native_in { units } else { U256::ZERO };
        if number("amountIn")? != units || number("transactionValue")? != value {
            return Err(SwapError::Untrusted("the route is for another amount"));
        }
        let calldata = text("data")?
            .strip_prefix("0x")
            .and_then(bahaa_types::hex::decode)
            .ok_or_else(unreadable)?;
        let described = described(&calldata).ok_or(SwapError::Untrusted(
            "the route asks for more than a plain swap",
        ))?;
        let ours = out
            .checked_mul(U256::from(WHOLE_BPS - SLIPPAGE_BPS))
            .map(|scaled| scaled / U256::from(WHOLE_BPS))
            .ok_or_else(unreadable)?;
        let honest = described.sold == sold
            && described.bought == bought
            && described.receiver == to
            && described.amount == units
            && described.minimum >= ours
            && described.minimum <= out;
        if !honest {
            return Err(SwapError::Untrusted(
                "the route pays someone else or less than it shows",
            ));
        }
        let amount =
            |units: U256| Amount::new(units, request.buy_decimals).map_err(|_| unreadable());
        let (expected, minimum) = (amount(out)?, amount(described.minimum)?);
        let route = Route {
            provider: Provider::KyberSwap.name(),
            method: Method::Aggregator,
            sell: request.sell.clone(),
            amount: request.amount,
            buy: request.buy.clone(),
            expected,
            minimum,
            costs: Vec::new(),
            wait: Some(Wait::blocks(1, chain.network().block_ms)),
        };
        let router = Address::Evm(self.router);
        let order = Order {
            provider: Provider::KyberSwap,
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
                    value: Amount::new(value, chain.network().native.decimals)
                        .map_err(|_| unreadable())?,
                    data: calldata,
                },
                spender: (!native_in).then_some(router),
                ours: false,
                spent: None,
            }),
            typed: None,
        };
        Ok((route, order))
    }

    async fn summary(
        &self,
        name: &str,
        sold: &[u8; 20],
        bought: &[u8; 20],
        units: U256,
    ) -> Result<serde_json::Value, SwapError> {
        let mut list_url = self.url(&format!("{name}/api/v1/routes"))?;
        list_url
            .query_pairs_mut()
            .append_pair("tokenIn", &hex(sold))
            .append_pair("tokenOut", &hex(bought))
            .append_pair("amountIn", &units.to_string())
            .append_pair("gasInclude", "true")
            .append_pair("excludeRFQSources", "true");
        let found = fetch(self.client.get(list_url)).await?;
        let summary = found["data"]["routeSummary"].clone();
        let same = |field: &str, wanted: &str| {
            summary[field]
                .as_str()
                .is_some_and(|text| text.eq_ignore_ascii_case(wanted))
        };
        if !same("tokenIn", &hex(sold))
            || !same("tokenOut", &hex(bought))
            || !same("amountIn", &units.to_string())
        {
            return Err(SwapError::Untrusted("the route is for another request"));
        }
        Ok(summary)
    }

    fn url(&self, path: &str) -> Result<Url, SwapError> {
        self.base.join(path).map_err(|_| unreadable())
    }
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
    let params = abi::tail(args, 0)?;
    let desc = abi::tail(params, 3)?;
    let clean = abi::length(desc, 4)? == 0
        && abi::length(desc, 5)? == 0
        && abi::length(desc, 10)? == 0
        && abi::uint(desc, 9)? & U256::from(PARTIAL_FILL) == U256::ZERO;
    clean.then_some(())?;
    Some(Described {
        sold: abi::address(desc, 0)?,
        bought: abi::address(desc, 1)?,
        receiver: abi::address(desc, 6)?,
        amount: abi::uint(desc, 7)?,
        minimum: abi::uint(desc, 8)?,
    })
}

#[cfg(fuzzing)]
pub(crate) fn fuzz(data: &[u8]) {
    let _ = described(data);
}

#[cfg(test)]
mod tests {
    use serde_json::Value;

    use super::*;

    fn fixture() -> Vec<u8> {
        let built: Value =
            serde_json::from_str(include_str!("../tests/fixtures/kyber-build.json")).unwrap();
        let text = built["data"]["data"].as_str().unwrap();
        bahaa_types::hex::decode(text.trim_start_matches("0x")).unwrap()
    }

    #[test]
    fn a_real_kyberswap_call_shows_its_receiver_amount_and_minimum() {
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
        assert_eq!(described.minimum.to_string(), "69341541821871455556");
    }

    #[test]
    fn a_call_with_a_fee_receiver_or_another_shape_is_not_read() {
        let mut other = fixture();
        other[0] = 0x8a;
        assert_eq!(described(&other), None);
        assert_eq!(described(&fixture()[..200]), None);
    }
}
