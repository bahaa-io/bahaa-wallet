use bahaa_chain::{Call, ChainError, Wait};
use bahaa_ton::{Body, TonChain};
use bahaa_types::{Address, Amount, AssetId, ChainFamily, NetworkId};
use reqwest::Client;
use ruint::aliases::U256;
use serde_json::Value;
use sha2::{Digest, Sha256};
use url::Url;

use crate::config::StonFiConfig;
use crate::order::{Contract, Order, Progress, Provider, QUOTE_LIFE, Request, SwapError, text_of};
use crate::reply::{fetch, unreadable};
use crate::route::{Charged, Cost, CostKind, Method, Route};

const NATIVE: &str = "EQAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAM9c";
const JETTON_TRANSFER: u64 = 0x0f8a_7ea5;
const SWAP_V1: u64 = 0x2593_8561;
const SWAP_V2: u64 = 0x6664_de2a;
const TON_TRANSFER_V2: u64 = 0x01f3_835d;
const SLIPPAGE_BPS: u32 = 50;
const WHOLE_BPS: u32 = 10_000;
const DEADLINE: u64 = 15 * 60;
const MILLI: u64 = 1_000_000;
const SWAP_SECONDS: u64 = 30;

#[derive(Clone, Copy)]
struct Router {
    address: Address,
    version: u8,
    pton: Address,
}

pub struct StonFi {
    client: Client,
    base: Url,
    network: NetworkId,
    routers: Vec<Router>,
}

struct Gas {
    attached: u64,
    forward: u64,
}

fn gas(version: u8, ton_in: bool, ton_out: bool) -> Gas {
    let milli = |value: u64| value * MILLI;
    match (version, ton_in, ton_out) {
        (1, true, _) => Gas {
            attached: milli(185),
            forward: milli(185),
        },
        (1, false, true) => Gas {
            attached: milli(170),
            forward: milli(125),
        },
        (1, false, false) => Gas {
            attached: milli(220),
            forward: milli(175),
        },
        (_, true, _) => Gas {
            attached: milli(310),
            forward: milli(300),
        },
        (_, false, _) => Gas {
            attached: milli(300),
            forward: milli(240),
        },
    }
}

impl StonFi {
    pub fn new(config: &StonFiConfig, client: Client) -> Result<Self, SwapError> {
        let broken = || SwapError::Untrusted("the built-in STON.fi routers are broken");
        let ton = |text: &str| Address::parse(ChainFamily::Ton, text).map_err(|_| broken());
        let routers = config
            .routers
            .iter()
            .map(|router| {
                if !matches!(router.version, 1 | 2) {
                    return Err(broken());
                }
                Ok(Router {
                    address: ton(&router.address)?,
                    version: router.version,
                    pton: ton(&router.pton)?,
                })
            })
            .collect::<Result<_, _>>()?;
        Ok(Self {
            client,
            base: config.url.clone(),
            network: config.network.clone(),
            routers,
        })
    }

    pub fn covers(&self, request: &Request) -> bool {
        *request.sell.network() == self.network
            && *request.buy.network() == self.network
            && request.sell != request.buy
            && request.from.family() == ChainFamily::Ton
            && request.to == request.from
    }

    pub async fn quote(
        &self,
        chain: &TonChain,
        request: &Request,
        now: u64,
    ) -> Result<(Route, Order), SwapError> {
        if !self.covers(request) {
            return Err(SwapError::Unsupported);
        }
        let units = request.amount.units();
        let mut url = self.base.join("swap/simulate").map_err(|_| unreadable())?;
        url.query_pairs_mut()
            .append_pair("offer_address", &asset_text(&request.sell))
            .append_pair("ask_address", &asset_text(&request.buy))
            .append_pair("units", &units.to_string())
            .append_pair("slippage_tolerance", "0.005")
            .append_pair("dex_v2", "true");
        let simulated = fetch(self.client.post(url)).await?;
        let pool = self.pool(chain, request, &simulated).await?;
        let minimum = pool
            .ask
            .checked_mul(U256::from(WHOLE_BPS - SLIPPAGE_BPS))
            .map(|scaled| scaled / U256::from(WHOLE_BPS))
            .ok_or_else(unreadable)?;
        let query = query_id(&request.from, now, units);
        let message =
            message(request, &pool, minimum, (now, query)).map_err(|err| network(&err))?;
        offer(request, &pool, minimum, message, now)
    }

    async fn pool(
        &self,
        chain: &TonChain,
        request: &Request,
        simulated: &Value,
    ) -> Result<Pool, SwapError> {
        let text = |field: &str| simulated[field].as_str().ok_or_else(unreadable);
        let number = |field: &str| -> Result<U256, SwapError> {
            text(field)?.parse().map_err(|_| unreadable())
        };
        let found = |field: &str| -> Result<Address, SwapError> {
            Address::parse(ChainFamily::Ton, text(field)?).map_err(|_| unreadable())
        };
        let router_address = found("router_address")?;
        let router = *self
            .routers
            .iter()
            .find(|router| router.address == router_address)
            .ok_or(SwapError::Untrusted(
                "the route goes through an unknown router",
            ))?;
        if number("offer_units")? != request.amount.units() {
            return Err(SwapError::Untrusted("the quote is for another amount"));
        }
        let wallet = |owner: Address, asset: &AssetId| {
            let master = asset.contract().copied().unwrap_or(router.pton);
            async move {
                chain
                    .jetton_wallet_of(&owner, &master)
                    .await
                    .map_err(|err| network(&err))
            }
        };
        let ask_wallet = wallet(router.address, &request.buy).await?;
        let router_offer = wallet(router.address, &request.sell).await?;
        if found("ask_jetton_wallet")? != ask_wallet
            || found("offer_jetton_wallet")? != router_offer
        {
            return Err(SwapError::Untrusted("the route names another pool"));
        }
        let offer_wallet = if request.sell.contract().is_none() {
            router_offer
        } else {
            wallet(request.from, &request.sell).await?
        };
        Ok(Pool {
            router,
            ask: number("ask_units")?,
            fee: number("fee_units").ok(),
            spent: simulated["gas_params"]["estimated_gas_consumption"]
                .as_str()
                .and_then(|text| text.parse().ok()),
            ask_wallet,
            offer_wallet,
        })
    }

    pub async fn status(&self, reference: &str) -> Result<Progress, SwapError> {
        let [router, owner, query] = reference.split(' ').collect::<Vec<_>>()[..] else {
            return Err(unreadable());
        };
        let mut url = self.base.join("swap/status").map_err(|_| unreadable())?;
        url.query_pairs_mut()
            .append_pair("router_address", router)
            .append_pair("owner_address", owner)
            .append_pair("query_id", query);
        let answer = fetch(self.client.get(url)).await?;
        match (answer["@type"].as_str(), answer["exitCode"].as_str()) {
            (Some("NotFound"), _) => Ok(Progress::Swapping),
            (Some("Found"), Some("swap_ok")) => Ok(Progress::Done),
            (Some("Found"), Some(_)) => Ok(Progress::Refunded),
            _ => Err(unreadable()),
        }
    }
}

struct Pool {
    router: Router,
    ask: U256,
    fee: Option<U256>,
    spent: Option<U256>,
    ask_wallet: Address,
    offer_wallet: Address,
}

fn message(
    request: &Request,
    pool: &Pool,
    minimum: U256,
    (now, query): (u64, u64),
) -> Result<(Vec<u8>, U256), ChainError> {
    let units = request.amount.units();
    let router = &pool.router;
    let ton_in = request.sell.contract().is_none();
    let gas = gas(router.version, ton_in, request.buy.contract().is_none());
    let swap = swap_body(
        router.version,
        &pool.ask_wallet,
        minimum,
        &request.from,
        now,
    )?;
    let body = match (ton_in, router.version) {
        (false, _) => jetton_transfer(
            query,
            units,
            &router.address,
            Some(&request.from),
            gas.forward,
            swap,
        )?,
        (true, 1) => jetton_transfer(query, units, &router.address, None, gas.forward, swap)?,
        (true, _) => pton_transfer(query, units, &request.from, swap)?,
    };
    let value = if ton_in {
        units
            .checked_add(U256::from(gas.attached))
            .ok_or_else(ChainError::overflow)?
    } else {
        U256::from(gas.attached)
    };
    Ok((body.boc()?, value))
}

fn offer(
    request: &Request,
    pool: &Pool,
    minimum: U256,
    (data, value): (Vec<u8>, U256),
    now: u64,
) -> Result<(Route, Order), SwapError> {
    let amount = |units: U256, decimals: u8| Amount::new(units, decimals).map_err(|_| unreadable());
    let expected = amount(pool.ask, request.buy_decimals)?;
    let minimum = amount(minimum, request.buy_decimals)?;
    let costs = pool
        .fee
        .map(|fee| {
            Ok::<_, SwapError>(Cost {
                kind: CostKind::PoolFee,
                asset: request.buy.clone(),
                amount: amount(fee, request.buy_decimals)?,
                charged: Charged::InQuote,
            })
        })
        .transpose()?
        .into_iter()
        .collect();
    let route = Route {
        provider: Provider::StonFi.name(),
        method: Method::Pool,
        sell: request.sell.clone(),
        amount: request.amount,
        buy: request.buy.clone(),
        expected,
        minimum,
        costs,
        wait: Some(Wait::About(SWAP_SECONDS)),
    };
    let query = query_id(&request.from, now, request.amount.units());
    let order = Order {
        provider: Provider::StonFi,
        reference: format!(
            "{} {} {query}",
            text_of(&pool.router.address, true),
            text_of(&request.from, false)
        ),
        deposit: pool.offer_wallet,
        comment: None,
        amount: request.amount,
        expected,
        minimum,
        expires_at: now.saturating_add(QUOTE_LIFE),
        contract: Some(Contract {
            call: Call {
                to: pool.offer_wallet,
                value: amount(value, request.native_decimals)?,
                data,
            },
            spender: None,
            ours: true,
            spent: pool
                .spent
                .map(|spent| amount(spent, request.native_decimals))
                .transpose()?,
        }),
        typed: None,
    };
    Ok((route, order))
}

fn network(err: &ChainError) -> SwapError {
    SwapError::Unreachable(err.to_string())
}

fn asset_text(asset: &AssetId) -> String {
    asset
        .contract()
        .map_or_else(|| NATIVE.to_owned(), |master| text_of(master, true))
}

fn query_id(owner: &Address, now: u64, units: U256) -> u64 {
    let digest = Sha256::new()
        .chain_update(owner.to_string())
        .chain_update(now.to_be_bytes())
        .chain_update(units.to_be_bytes::<32>())
        .finalize();
    let mut bytes = [0u8; 8];
    bytes.copy_from_slice(&digest[..8]);
    u64::from_be_bytes(bytes) >> 1
}

fn jetton_transfer(
    query: u64,
    units: U256,
    destination: &Address,
    response: Option<&Address>,
    forward: u64,
    payload: Body,
) -> Result<Body, ChainError> {
    Body::default()
        .uint(32, JETTON_TRANSFER)?
        .uint(64, query)?
        .coins(units)?
        .address(Some(destination))?
        .address(response)?
        .bit(false)?
        .coins(U256::from(forward))?
        .bit(true)?
        .reference(payload)
}

fn pton_transfer(
    query: u64,
    units: U256,
    refund: &Address,
    payload: Body,
) -> Result<Body, ChainError> {
    Body::default()
        .uint(32, TON_TRANSFER_V2)?
        .uint(64, query)?
        .coins(units)?
        .address(Some(refund))?
        .bit(true)?
        .reference(payload)
}

fn swap_body(
    version: u8,
    ask_wallet: &Address,
    minimum: U256,
    owner: &Address,
    now: u64,
) -> Result<Body, ChainError> {
    if version == 1 {
        return Body::default()
            .uint(32, SWAP_V1)?
            .address(Some(ask_wallet))?
            .coins(minimum)?
            .address(Some(owner))?
            .bit(false);
    }
    let details = Body::default()
        .coins(minimum)?
        .address(Some(owner))?
        .coins(U256::ZERO)?
        .bit(false)?
        .coins(U256::ZERO)?
        .bit(false)?
        .uint(16, 0)?
        .address(None)?;
    Body::default()
        .uint(32, SWAP_V2)?
        .address(Some(ask_wallet))?
        .address(Some(owner))?
        .address(Some(owner))?
        .uint(64, now.saturating_add(DEADLINE))?
        .reference(details)
}

#[cfg(test)]
mod tests {
    use serde_json::Value;

    use super::*;

    fn ton(text: &str) -> Address {
        Address::parse(ChainFamily::Ton, text).unwrap()
    }

    fn built(name: &str) -> Value {
        include_str!("../tests/fixtures/stonfi-sdk.jsonl")
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .find(|line| line["name"] == name)
            .unwrap()
    }

    fn case(
        version: u8,
        (sell, sold, decimals): (&str, &str, u8),
        buy: &str,
        (ask_wallet, offer_wallet): (&str, &str),
        minimum: u64,
    ) -> (String, String) {
        let user = ton("UQCi1ivY6QriyKIQZrSTwhvf4qXI19qIPe4XAOYo-qMMUEiR");
        let (router, pton) = if version == 1 {
            (
                "EQB3ncyBUTjZUA5EnFKR5_EnOMI9V1tTEAAPaiU71gc4TiUt",
                "EQCM3B12QK1e4yZSf8GtBRT0aLMNyEsBc_DhVfRRtOEffLez",
            )
        } else {
            (
                "EQCS4UEa5UaJLzOyyKieqQOQ2P9M-7kXpkO5HnP3Bv250cN3",
                "EQBnGWMCf3-FZZq1W4IWcWiGAc3PHuZ0_H-7sad2oY00o83S",
            )
        };
        let request = Request {
            sell: sell.parse().unwrap(),
            amount: Amount::parse(sold, decimals).unwrap(),
            buy: buy.parse().unwrap(),
            buy_decimals: 6,
            native_decimals: 9,
            from: user,
            to: user,
            deadline: 1_800_001_800,
        };
        let pool = Pool {
            router: Router {
                address: ton(router),
                version,
                pton: ton(pton),
            },
            ask: U256::ZERO,
            fee: None,
            spent: None,
            ask_wallet: ton(ask_wallet),
            offer_wallet: ton(offer_wallet),
        };
        let (body, value) = message(
            &request,
            &pool,
            U256::from(minimum),
            (1_800_000_000, 12_345),
        )
        .unwrap();
        (bahaa_types::hex::encode(&body), value.to_string())
    }

    const USDT: &str = "ton/EQCxE6mUtQJKFnGfaROTKOt1lZbDiiX1kCixRv7Nw2Id_sDs";

    #[test]
    fn our_messages_match_the_official_sdk_byte_for_byte() {
        for (name, ours) in [
            (
                "v1-ton-in",
                case(
                    1,
                    ("ton", "542", 9),
                    USDT,
                    (
                        "EQBO7JIbnU1WoNlGdgFtScJrObHXkBp-FT5mAz8UagiG9KQR",
                        "EQARULUYsmJq1RiZ-YiH-IJLcAZUVkVff-KBPwEmmaQGH6aC",
                    ),
                    632_140_847,
                ),
            ),
            (
                "v1-jetton-in",
                case(
                    1,
                    (USDT, "100", 6),
                    "ton",
                    (
                        "EQARULUYsmJq1RiZ-YiH-IJLcAZUVkVff-KBPwEmmaQGH6aC",
                        "EQCSLWJ9fY7b0A5OI72wxUp27l4fRlc6GvRBeFf6PiPpH4p3",
                    ),
                    84_471_325_489,
                ),
            ),
            (
                "v2-ton-in",
                case(
                    2,
                    ("ton", "542", 9),
                    USDT,
                    (
                        "EQCSLWJ9fY7b0A5OI72wxUp27l4fRlc6GvRBeFf6PiPpH4p3",
                        "EQCSIMGBps_qzRG3uPYhON8bucyCtu0mYdL1-u4gSz77IBa3",
                    ),
                    632_140_847,
                ),
            ),
            (
                "v2-jetton-in",
                case(
                    2,
                    (USDT, "100", 6),
                    "ton",
                    (
                        "EQCSIMGBps_qzRG3uPYhON8bucyCtu0mYdL1-u4gSz77IBa3",
                        "EQCSLWJ9fY7b0A5OI72wxUp27l4fRlc6GvRBeFf6PiPpH4p3",
                    ),
                    84_471_325_489,
                ),
            ),
        ] {
            let sdk = built(name);
            assert_eq!(ours.0, sdk["body"].as_str().unwrap(), "{name} body");
            assert_eq!(ours.1, sdk["value"].as_str().unwrap(), "{name} value");
        }
    }

    #[test]
    fn query_ids_differ_per_request_and_fit_63_bits() {
        let owner = Address::Ton {
            workchain: 0,
            hash: [1; 32],
        };
        let first = query_id(&owner, 1, U256::from(5u8));
        assert_ne!(first, query_id(&owner, 2, U256::from(5u8)));
        assert!(first < 1 << 63);
    }

    #[test]
    fn gas_follows_the_sdk_for_each_router_and_direction() {
        assert_eq!(gas(1, true, false).forward, 185_000_000);
        assert_eq!(gas(1, false, true).attached, 170_000_000);
        assert_eq!(gas(2, true, false).attached, 310_000_000);
        assert_eq!(gas(2, false, false).forward, 240_000_000);
    }
}
