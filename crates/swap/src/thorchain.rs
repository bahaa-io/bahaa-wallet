use std::collections::BTreeMap;

use bahaa_chain::{Call, Wait};
use bahaa_types::{Address, Amount, AssetId, ChainFamily, NetworkId};
use reqwest::Client;
use ruint::aliases::U256;
use serde_json::Value;
use url::Url;

use crate::abi;
use crate::config::{ThorAsset, ThorchainConfig};
use crate::order::{Contract, Order, Progress, Provider, QUOTE_LIFE, Request, SwapError};
use crate::reply::{fetch, unreadable};
use crate::route::{Charged, Cost, CostKind, Method, Route};

const DEPOSIT: [u8; 4] = [0x44, 0xbc, 0x93, 0x7b];
const UNITS: u8 = 8;
const SLIPPAGE_BPS: u32 = 100;
const WHOLE_BPS: u32 = 10_000;
const MAX_EXPIRY: u64 = 3_600;
const MAX_STREAMING: u64 = 100;

pub struct Thorchain {
    client: Client,
    base: Url,
    network: NetworkId,
    assets: BTreeMap<AssetId, ThorAsset>,
    routers: BTreeMap<String, Address>,
}

struct Pool {
    vault: [u8; 20],
    router: [u8; 20],
}

impl Thorchain {
    pub fn new(config: &ThorchainConfig, client: Client) -> Result<Self, SwapError> {
        let routers = config
            .routers
            .iter()
            .map(|(chain, router)| {
                Address::parse(ChainFamily::Evm, router)
                    .map(|router| (chain.clone(), router))
                    .map_err(|_| SwapError::Untrusted("the built-in THORChain routers are broken"))
            })
            .collect::<Result<_, _>>()?;
        Ok(Self {
            client,
            base: config.url.clone(),
            network: config.network.clone(),
            assets: config.assets.clone(),
            routers,
        })
    }

    pub fn covers(&self, request: &Request) -> bool {
        let routed = self
            .assets
            .get(&request.sell)
            .is_some_and(|asset| self.routers.contains_key(chain_of(&asset.pool)));
        request.buy == AssetId::Native(self.network.clone())
            && self.assets.contains_key(&request.buy)
            && routed
            && matches!(request.from, Address::Evm(_))
            && request.to.family() == ChainFamily::Bitcoin
    }

    pub async fn quote(&self, request: &Request, now: u64) -> Result<(Route, Order), SwapError> {
        let (Some(sold), Some(bought)) = (
            self.assets
                .get(&request.sell)
                .filter(|_| self.covers(request)),
            self.assets.get(&request.buy),
        ) else {
            return Err(SwapError::Unsupported);
        };
        let chain = chain_of(&sold.pool);
        let pool = self.pool(chain, chain_of(&bought.pool)).await?;
        let amount =
            thor_units(request.amount.units(), sold.decimals).ok_or(SwapError::Unsupported)?;
        let destination = request.to.to_string();
        let mut url = self
            .base
            .join("thorchain/quote/swap")
            .map_err(|_| unreadable())?;
        url.query_pairs_mut()
            .append_pair("from_asset", &sold.pool)
            .append_pair("to_asset", &bought.pool)
            .append_pair("amount", &amount.to_string())
            .append_pair("destination", &destination);
        let quote = fetch(self.client.get(url)).await?;
        let text = |field: &str| quote[field].as_str().ok_or_else(unreadable);
        let least: U256 = text("recommended_min_amount_in")?
            .parse()
            .map_err(|_| unreadable())?;
        if amount < least {
            return Err(SwapError::Refused(format!(
                "THORChain needs at least {} {} for this swap, so a refund can be paid",
                least_amount(least, sold.decimals).ok_or_else(unreadable)?,
                ticker(&sold.pool)
            )));
        }
        let vault = match Address::parse(
            ChainFamily::Evm,
            &text("inbound_address")?.to_ascii_lowercase(),
        ) {
            Ok(Address::Evm(vault)) if vault == pool.vault => vault,
            _ => {
                return Err(SwapError::Untrusted(
                    "the quote deposits into another vault",
                ));
            }
        };
        let router = match Address::parse(ChainFamily::Evm, &text("router")?.to_ascii_lowercase()) {
            Ok(Address::Evm(router)) if router == pool.router => router,
            _ => return Err(SwapError::Untrusted("the quote calls an unknown router")),
        };
        let expected: U256 = text("expected_amount_out")?
            .parse()
            .map_err(|_| unreadable())?;
        let minimum = expected
            .checked_mul(U256::from(WHOLE_BPS - SLIPPAGE_BPS))
            .map(|scaled| scaled / U256::from(WHOLE_BPS))
            .filter(|minimum| !minimum.is_zero())
            .ok_or_else(unreadable)?;
        let memo = memo(text("memo")?, bought, &destination, minimum)
            .ok_or(SwapError::Untrusted("the quote's memo pays someone else"))?;
        let expiry = quote["expiry"]
            .as_u64()
            .filter(|expiry| *expiry > now && *expiry <= now.saturating_add(MAX_EXPIRY))
            .ok_or(SwapError::Untrusted("the quote has an impossible expiry"))?;
        let token = match request.sell.contract() {
            None => [0u8; 20],
            Some(Address::Evm(token)) => *token,
            Some(_) => return Err(SwapError::Unsupported),
        };
        let data = deposit_data(&vault, &token, request.amount.units(), &memo, expiry);
        let receive = |units: U256| Amount::new(units, UNITS).map_err(|_| unreadable());
        let route = Route {
            provider: Provider::Thorchain.name(),
            method: Method::Validators,
            sell: request.sell.clone(),
            amount: request.amount,
            buy: request.buy.clone(),
            expected: receive(expected)?,
            minimum: receive(minimum)?,
            costs: costs(&quote["fees"], &request.buy),
            wait: quote["total_swap_seconds"].as_u64().map(Wait::About),
        };
        let order = order(request, &route, (Address::Evm(router), data), now)?;
        Ok((route, order))
    }

    pub async fn status(&self, transaction: &str) -> Result<Progress, SwapError> {
        let hash = transaction.trim_start_matches("0x").to_ascii_uppercase();
        if hash.len() != 64 || bahaa_types::hex::decode(&hash.to_ascii_lowercase()).is_none() {
            return Err(unreadable());
        }
        let url = self
            .base
            .join(&format!("thorchain/tx/status/{hash}"))
            .map_err(|_| unreadable())?;
        progress(&fetch(self.client.get(url)).await?)
    }

    async fn pool(&self, source: &str, target: &str) -> Result<Pool, SwapError> {
        let url = self
            .base
            .join("thorchain/inbound_addresses")
            .map_err(|_| unreadable())?;
        let listed = fetch(self.client.get(url)).await?;
        let entry = |chain: &str| {
            listed
                .as_array()
                .and_then(|entries| entries.iter().find(|entry| entry["chain"] == chain))
        };
        let open = |chain: &str| {
            entry(chain).is_some_and(|entry| {
                ["halted", "chain_trading_paused", "global_trading_paused"]
                    .iter()
                    .all(|flag| entry[*flag] != true)
            })
        };
        if !open(source) || !open(target) {
            return Err(SwapError::Refused(
                "THORChain has paused trading on this network".to_owned(),
            ));
        }
        let source_entry = entry(source).ok_or_else(unreadable)?;
        let evm = |field: &str| match source_entry[field]
            .as_str()
            .map(|text| Address::parse(ChainFamily::Evm, &text.to_ascii_lowercase()))
        {
            Some(Ok(Address::Evm(bytes))) => Ok(bytes),
            _ => Err(unreadable()),
        };
        let router = evm("router")?;
        if self.routers.get(source) != Some(&Address::Evm(router)) {
            return Err(SwapError::Untrusted("THORChain lists an unknown router"));
        }
        Ok(Pool {
            vault: evm("address")?,
            router,
        })
    }
}

fn order(
    request: &Request,
    route: &Route,
    (router, data): (Address, Vec<u8>),
    now: u64,
) -> Result<Order, SwapError> {
    let native_in = request.sell.contract().is_none();
    Ok(Order {
        provider: Provider::Thorchain,
        reference: String::new(),
        deposit: router,
        comment: None,
        amount: request.amount,
        expected: route.expected,
        minimum: route.minimum,
        expires_at: now.saturating_add(QUOTE_LIFE),
        contract: Some(Contract {
            call: Call {
                to: router,
                value: Amount::new(
                    if native_in {
                        request.amount.units()
                    } else {
                        U256::ZERO
                    },
                    request.native_decimals,
                )
                .map_err(|_| unreadable())?,
                data,
            },
            spender: (!native_in).then_some(router),
            ours: true,
            spent: None,
        }),
        typed: None,
    })
}

fn chain_of(pool: &str) -> &str {
    pool.split('.').next().unwrap_or(pool)
}

fn thor_units(units: U256, decimals: u8) -> Option<U256> {
    match decimals.checked_sub(UNITS) {
        Some(extra) => Some(units / U256::from(10u8).checked_pow(U256::from(extra))?),
        None => units.checked_mul(U256::from(10u8).checked_pow(U256::from(UNITS - decimals))?),
    }
    .filter(|units| !units.is_zero())
}

fn least_amount(units: U256, decimals: u8) -> Option<Amount> {
    let scaled = match decimals.checked_sub(UNITS) {
        Some(extra) => units.checked_mul(U256::from(10u8).checked_pow(U256::from(extra))?)?,
        None => units.div_ceil(U256::from(10u8).checked_pow(U256::from(UNITS - decimals))?),
    };
    Amount::new(scaled, decimals).ok()
}

fn ticker(pool: &str) -> &str {
    let asset = pool.split_once('.').map_or(pool, |(_, asset)| asset);
    asset.split_once('-').map_or(asset, |(symbol, _)| symbol)
}

fn memo(quoted: &str, bought: &ThorAsset, destination: &str, minimum: U256) -> Option<String> {
    let parts: Vec<&str> = quoted.split(':').collect();
    let ([action, asset, address] | [action, asset, address, _]) = parts.as_slice() else {
        return None;
    };
    let names = [Some(bought.pool.as_str()), bought.short.as_deref()];
    if !matches!(*action, "=" | "SWAP" | "s")
        || !names.contains(&Some(*asset))
        || *address != destination
    {
        return None;
    }
    let Some(streaming) = parts.get(3) else {
        return Some(format!("=:{asset}:{destination}:{minimum}"));
    };
    let numbers: Vec<u64> = streaming
        .split('/')
        .map(|number| number.parse().ok())
        .collect::<Option<_>>()?;
    let [_, interval, quantity] = numbers.as_slice() else {
        return None;
    };
    (*interval <= MAX_STREAMING && *quantity <= MAX_STREAMING)
        .then(|| format!("=:{asset}:{destination}:{minimum}/{interval}/{quantity}"))
}

fn deposit_data(
    vault: &[u8; 20],
    token: &[u8; 20],
    units: U256,
    memo: &str,
    expiry: u64,
) -> Vec<u8> {
    let mut data = DEPOSIT.to_vec();
    abi::push_address(&mut data, vault);
    abi::push_address(&mut data, token);
    abi::push_uint(&mut data, units);
    abi::push_uint(&mut data, U256::from(160u8));
    abi::push_uint(&mut data, U256::from(expiry));
    abi::push_uint(&mut data, U256::from(memo.len()));
    data.extend_from_slice(memo.as_bytes());
    data.resize(data.len() + (32 - memo.len() % 32) % 32, 0);
    data
}

fn costs(fees: &Value, bought: &AssetId) -> Vec<Cost> {
    [
        ("outbound", CostKind::NetworkFee),
        ("liquidity", CostKind::PoolFee),
    ]
    .into_iter()
    .filter_map(|(field, kind)| {
        let units: U256 = fees[field].as_str()?.parse().ok()?;
        let amount = Amount::new(units, UNITS)
            .ok()
            .filter(|amount| !amount.is_zero())?;
        Some(Cost {
            kind,
            asset: bought.clone(),
            amount,
            charged: Charged::InQuote,
        })
    })
    .collect()
}

fn progress(status: &Value) -> Result<Progress, SwapError> {
    let stages = &status["stages"];
    let done = |stage: &str| stages[stage]["completed"] == true;
    if !stages.is_object() {
        return Err(unreadable());
    }
    let refunded = status["out_txs"]
        .as_array()
        .into_iter()
        .flatten()
        .any(|out| {
            out["memo"]
                .as_str()
                .is_some_and(|memo| memo.starts_with("REFUND:"))
        })
        || status["planned_out_txs"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|out| out["refund"] == true);
    Ok(if !done("inbound_observed") {
        Progress::Waiting
    } else if refunded && done("swap_finalised") {
        Progress::Refunded
    } else if done("outbound_signed") {
        Progress::Done
    } else if done("inbound_finalised") {
        Progress::Swapping
    } else {
        Progress::Received
    })
}

#[cfg(fuzzing)]
pub(crate) fn fuzz(value: &Value) {
    let _ = progress(value);
    if let Ok(bitcoin) = "bitcoin".parse::<AssetId>() {
        let _ = costs(&value["fees"], &bitcoin);
    }
    if let Some(quoted) = value["memo"].as_str() {
        let btc = ThorAsset {
            pool: "BTC.BTC".to_owned(),
            short: Some("b".to_owned()),
            decimals: 8,
        };
        let destination = value["destination"].as_str().unwrap_or_default();
        let _ = memo(quoted, &btc, destination, U256::from(1u8));
    }
}

#[cfg(test)]
mod tests {
    use sha3::{Digest, Keccak256};

    use super::*;

    fn fixture(name: &str) -> Value {
        let text = match name {
            "done" => include_str!("../tests/fixtures/thorchain-done.json"),
            _ => include_str!("../tests/fixtures/thorchain-refund.json"),
        };
        serde_json::from_str(text).unwrap()
    }

    #[test]
    fn the_router_call_is_deposit_with_expiry() {
        let selector =
            Keccak256::digest(b"depositWithExpiry(address,address,uint256,string,uint256)");
        assert_eq!(selector[..4], DEPOSIT);
        let data = deposit_data(&[1; 20], &[0; 20], U256::from(5u8), "=:b:bc1q:9/0/3", 77);
        assert_eq!(data.len(), 4 + 7 * 32);
        assert_eq!(abi::uint(&data[4..], 3), Some(U256::from(160u8)));
        assert_eq!(abi::uint(&data[4..], 5), Some(U256::from(14u8)));
    }

    #[test]
    fn our_memo_keeps_the_route_and_sets_our_own_limit() {
        let btc = ThorAsset {
            pool: "BTC.BTC".to_owned(),
            short: Some("b".to_owned()),
            decimals: 8,
        };
        let quoted = "=:b:bc1qcr8te4kr609gcawutmrza0j4xv80jy8z306fyu:0/0/9";
        assert_eq!(
            memo(
                quoted,
                &btc,
                "bc1qcr8te4kr609gcawutmrza0j4xv80jy8z306fyu",
                U256::from(289_481u32)
            ),
            Some("=:b:bc1qcr8te4kr609gcawutmrza0j4xv80jy8z306fyu:289481/0/9".to_owned())
        );
        assert_eq!(
            memo(
                quoted,
                &btc,
                "bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t4",
                U256::from(1u8)
            ),
            None
        );
        assert_eq!(
            memo(
                "=:b:bc1qcr8te4kr609gcawutmrza0j4xv80jy8z306fyu",
                &btc,
                "bc1qcr8te4kr609gcawutmrza0j4xv80jy8z306fyu",
                U256::from(108_000u32)
            ),
            Some("=:b:bc1qcr8te4kr609gcawutmrza0j4xv80jy8z306fyu:108000".to_owned())
        );
        assert_eq!(memo("=:e:bc1q:0/0/9", &btc, "bc1q", U256::from(1u8)), None);
        assert_eq!(
            memo("=:b:bc1q:0/0/9:affiliate", &btc, "bc1q", U256::from(1u8)),
            None
        );
    }

    #[test]
    fn the_recommended_minimum_reads_back_in_the_sold_asset() {
        assert_eq!(
            least_amount(U256::from(209_526u32), 18)
                .unwrap()
                .to_string(),
            "0.00209526"
        );
        assert_eq!(
            least_amount(U256::from(1_000_000_001u64), 6)
                .unwrap()
                .to_string(),
            "10.000001"
        );
        assert_eq!(
            ticker("ETH.USDC-0XA0B86991C6218B36C1D19D4A2E9EB0CE3606EB48"),
            "USDC"
        );
        assert_eq!(ticker("BASE.ETH"), "ETH");
    }

    #[test]
    fn amounts_move_to_and_from_thorchain_units() {
        assert_eq!(
            thor_units(U256::from(100_000_000_000_000_000u64), 18),
            Some(U256::from(10_000_000u32))
        );
        assert_eq!(
            thor_units(U256::from(5_000_000u32), 6),
            Some(U256::from(500_000_000u32))
        );
        assert_eq!(thor_units(U256::from(1u8), 18), None);
    }

    #[test]
    fn recorded_statuses_read_as_done_and_refunded() {
        assert_eq!(progress(&fixture("done")).unwrap(), Progress::Done);
        assert_eq!(progress(&fixture("refund")).unwrap(), Progress::Refunded);
        let unseen = serde_json::json!({"stages": {"inbound_observed": {"completed": false}}});
        assert_eq!(progress(&unseen).unwrap(), Progress::Waiting);
    }
}
