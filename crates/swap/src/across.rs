use std::collections::BTreeMap;

use bahaa_chain::{Call, Chain, Wait};
use bahaa_evm::EvmChain;
use bahaa_types::{Address, Amount, AssetId, ChainFamily, NetworkId};
use reqwest::Client;
use ruint::aliases::U256;
use serde_json::Value;
use url::Url;

use crate::config::AcrossConfig;
use crate::order::{Contract, Order, Progress, Provider, QUOTE_LIFE, Request, SwapError};
use crate::reply::{fetch, unreadable};
use crate::route::{Charged, Cost, CostKind, Method, Route};
use bahaa_chain::abi;

const DEPOSIT: [u8; 4] = [0x7b, 0x93, 0x92, 0x32];
const HEAD_WORDS: usize = 12;
const MAX_FILL_WINDOW: u64 = 86_400;
const MAX_QUOTE_AGE: u64 = 3_600;

struct Spoke {
    chain: u64,
    address: [u8; 20],
    wrapped: [u8; 20],
}

pub struct Across {
    client: Client,
    base: Url,
    spokes: BTreeMap<NetworkId, Spoke>,
    classes: BTreeMap<AssetId, String>,
}

impl Across {
    pub fn new(config: &AcrossConfig, client: Client) -> Result<Self, SwapError> {
        let broken = || SwapError::Untrusted("the built-in Across list is broken");
        let evm = |text: &str| match Address::parse(ChainFamily::Evm, text) {
            Ok(Address::Evm(bytes)) => Ok(bytes),
            _ => Err(broken()),
        };
        let spokes = config
            .spokes
            .iter()
            .map(|(network, spoke)| {
                Ok((
                    network.clone(),
                    Spoke {
                        chain: spoke.chain,
                        address: evm(&spoke.address)?,
                        wrapped: evm(&spoke.wrapped)?,
                    },
                ))
            })
            .collect::<Result<_, SwapError>>()?;
        Ok(Self {
            client,
            base: config.url.clone(),
            spokes,
            classes: config.assets.clone(),
        })
    }

    pub fn covers(&self, request: &Request) -> bool {
        let class = |asset: &AssetId| self.classes.get(asset);
        request.sell.network() != request.buy.network()
            && self.spokes.contains_key(request.sell.network())
            && self.spokes.contains_key(request.buy.network())
            && class(&request.sell).is_some()
            && class(&request.sell) == class(&request.buy)
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
        let (Some(origin), Some(target)) = (
            self.spokes.get(request.sell.network()),
            self.spokes.get(request.buy.network()),
        ) else {
            return Err(SwapError::Unsupported);
        };
        let (Address::Evm(from), Address::Evm(to)) = (request.from, request.to) else {
            return Err(SwapError::Unsupported);
        };
        if !self.covers(request) {
            return Err(SwapError::Unsupported);
        }
        let sold = token(origin, &request.sell)?;
        let bought = token(target, &request.buy)?;
        let units = request.amount.units();
        let mut url = self.base.join("suggested-fees").map_err(|_| unreadable())?;
        url.query_pairs_mut()
            .append_pair("inputToken", &hex(&sold))
            .append_pair("outputToken", &hex(&bought))
            .append_pair("originChainId", &origin.chain.to_string())
            .append_pair("destinationChainId", &target.chain.to_string())
            .append_pair("amount", &units.to_string())
            .append_pair("depositor", &hex(&from))
            .append_pair("recipient", &hex(&to));
        let quote = fetch(self.client.get(url)).await?;
        let terms = Terms::read(&quote, origin, now)?;
        if terms.too_low {
            return Err(SwapError::Refused(
                "the amount is below Across's minimum".to_owned(),
            ));
        }
        if terms.output.is_zero() || terms.output > units {
            return Err(SwapError::Untrusted("the quote is for another amount"));
        }
        let data = deposit_data(&Deposit {
            from,
            to,
            sold,
            bought,
            units,
            terms: &terms,
            destination: target.chain,
        });
        let native_in = request.sell.contract().is_none();
        let fee = units.checked_sub(terms.output).ok_or_else(unreadable)?;
        let output = Amount::new(terms.output, request.buy_decimals).map_err(|_| unreadable())?;
        let route = Route {
            provider: Provider::Across.name(),
            method: Method::Escrow,
            sell: request.sell.clone(),
            amount: request.amount,
            buy: request.buy.clone(),
            expected: output,
            minimum: output,
            costs: vec![Cost {
                kind: CostKind::ProviderFee,
                asset: request.sell.clone(),
                amount: Amount::new(fee, request.amount.decimals()).map_err(|_| unreadable())?,
                charged: Charged::InQuote,
            }],
            wait: Some(Wait::About(terms.seconds.saturating_add(
                u64::from(chain.network().block_ms).div_ceil(1_000),
            ))),
        };
        let spoke = Address::Evm(origin.address);
        let value = Amount::new(
            if native_in { units } else { U256::ZERO },
            chain.network().native.decimals,
        )
        .map_err(|_| unreadable())?;
        let order = Order {
            provider: Provider::Across,
            reference: String::new(),
            deposit: spoke,
            comment: None,
            amount: request.amount,
            expected: output,
            minimum: output,
            expires_at: now.saturating_add(QUOTE_LIFE),
            contract: Some(Contract {
                call: Call {
                    to: spoke,
                    value,
                    data,
                },
                spender: (!native_in).then_some(spoke),
                ours: true,
                spent: None,
            }),
            typed: None,
        };
        Ok((route, order))
    }

    pub async fn status(
        &self,
        network: &NetworkId,
        transaction: &str,
    ) -> Result<Progress, SwapError> {
        let spoke = self.spokes.get(network).ok_or(SwapError::Unsupported)?;
        let mut url = self.base.join("deposit/status").map_err(|_| unreadable())?;
        url.query_pairs_mut()
            .append_pair("originChainId", &spoke.chain.to_string())
            .append_pair("depositTxHash", transaction);
        let answer = fetch(self.client.get(url)).await?;
        match answer["status"].as_str() {
            Some("pending" | "slowFillRequested") => Ok(Progress::Swapping),
            Some("filled") => Ok(Progress::Done),
            Some("refunded") => Ok(Progress::Refunded),
            Some("expired") => Ok(Progress::Failed(
                "Across did not fill the deposit in time, a refund follows".to_owned(),
            )),
            _ => Err(SwapError::Untrusted(
                "Across sent a status this wallet can't read",
            )),
        }
    }
}

struct Terms {
    output: U256,
    relayer: [u8; 20],
    stamp: u32,
    deadline: u32,
    exclusivity: u32,
    seconds: u64,
    too_low: bool,
}

impl Terms {
    fn read(quote: &Value, origin: &Spoke, now: u64) -> Result<Self, SwapError> {
        let text = |field: &str| quote[field].as_str().ok_or_else(unreadable);
        let spoke = Address::parse(ChainFamily::Evm, text("spokePoolAddress")?)
            .map_err(|_| unreadable())?;
        if spoke != Address::Evm(origin.address) {
            return Err(SwapError::Untrusted(
                "the quote deposits into an unknown contract",
            ));
        }
        let small = |value: &Value| -> Result<u64, SwapError> {
            match value {
                Value::String(text) => text.parse().map_err(|_| unreadable()),
                Value::Number(number) => number.as_u64().ok_or_else(unreadable),
                _ => Err(unreadable()),
            }
        };
        let stamp = small(&quote["timestamp"])?;
        let deadline = small(&quote["fillDeadline"])?;
        let fresh = stamp <= now.saturating_add(MAX_QUOTE_AGE)
            && stamp.saturating_add(MAX_QUOTE_AGE) >= now
            && deadline > now
            && deadline <= now.saturating_add(MAX_FILL_WINDOW);
        if !fresh {
            return Err(SwapError::Untrusted("the quote has impossible times"));
        }
        let Ok(Address::Evm(relayer)) = Address::parse(ChainFamily::Evm, text("exclusiveRelayer")?)
        else {
            return Err(unreadable());
        };
        let narrow = |value: u64| u32::try_from(value).map_err(|_| unreadable());
        Ok(Self {
            output: text("outputAmount")?.parse().map_err(|_| unreadable())?,
            relayer,
            stamp: narrow(stamp)?,
            deadline: narrow(deadline)?,
            exclusivity: narrow(small(&quote["exclusivityDeadline"])?)?,
            seconds: small(&quote["estimatedFillTimeSec"])?,
            too_low: quote["isAmountTooLow"].as_bool().ok_or_else(unreadable)?,
        })
    }
}

struct Deposit<'a> {
    from: [u8; 20],
    to: [u8; 20],
    sold: [u8; 20],
    bought: [u8; 20],
    units: U256,
    terms: &'a Terms,
    destination: u64,
}

fn deposit_data(deposit: &Deposit<'_>) -> Vec<u8> {
    let mut data = DEPOSIT.to_vec();
    abi::push_address(&mut data, &deposit.from);
    abi::push_address(&mut data, &deposit.to);
    abi::push_address(&mut data, &deposit.sold);
    abi::push_address(&mut data, &deposit.bought);
    abi::push_uint(&mut data, deposit.units);
    abi::push_uint(&mut data, deposit.terms.output);
    abi::push_uint(&mut data, U256::from(deposit.destination));
    abi::push_address(&mut data, &deposit.terms.relayer);
    abi::push_uint(&mut data, U256::from(deposit.terms.stamp));
    abi::push_uint(&mut data, U256::from(deposit.terms.deadline));
    abi::push_uint(&mut data, U256::from(deposit.terms.exclusivity));
    abi::push_uint(&mut data, U256::from(HEAD_WORDS * 32));
    abi::push_uint(&mut data, U256::ZERO);
    data
}

fn token(spoke: &Spoke, asset: &AssetId) -> Result<[u8; 20], SwapError> {
    match asset.contract() {
        None => Ok(spoke.wrapped),
        Some(Address::Evm(bytes)) => Ok(*bytes),
        Some(_) => Err(SwapError::Unsupported),
    }
}

fn hex(bytes: &[u8; 20]) -> String {
    format!("0x{}", bahaa_types::hex::encode(bytes))
}

#[cfg(fuzzing)]
pub(crate) fn fuzz(value: &Value) {
    let origin = Spoke {
        chain: 1,
        address: [1; 20],
        wrapped: [2; 20],
    };
    let _ = Terms::read(value, &origin, 0);
}
