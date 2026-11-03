use std::collections::BTreeMap;

use bahaa_chain::{Call, Wait};
use bahaa_types::{Address, Amount, AssetId, ChainFamily, NetworkId};
use reqwest::Client;
use ruint::aliases::U256;
use serde_json::Value;
use url::Url;

use crate::abi;
use crate::config::CircleConfig;
use crate::order::{Contract, Order, Progress, Provider, QUOTE_LIFE, Request, SwapError};
use crate::reply::{fetch, unreadable};
use crate::route::{Charged, Cost, CostKind, Method, Route};

const DEPOSIT: [u8; 4] = [0x77, 0x9b, 0x43, 0x2d];
const FAST: u32 = 1_000;
const FORWARD: [u8; 12] = *b"cctp-forward";
const HUNDREDTH_BPS: u32 = 1_000_000;
const FAST_SECONDS: u64 = 60;

pub struct Circle {
    client: Client,
    base: Url,
    messenger: [u8; 20],
    domains: BTreeMap<AssetId, u32>,
}

struct Fees {
    fast: U256,
    expected: U256,
    max: U256,
}

impl Circle {
    pub fn new(config: &CircleConfig, client: Client) -> Result<Self, SwapError> {
        let Ok(Address::Evm(messenger)) = Address::parse(ChainFamily::Evm, &config.messenger)
        else {
            return Err(SwapError::Untrusted(
                "the built-in Circle contract is broken",
            ));
        };
        Ok(Self {
            client,
            base: config.url.clone(),
            messenger,
            domains: config.usdc.clone(),
        })
    }

    pub fn covers(&self, request: &Request) -> bool {
        self.domains.contains_key(&request.sell)
            && self.domains.contains_key(&request.buy)
            && request.sell.network() != request.buy.network()
            && matches!(
                (request.from, request.to),
                (Address::Evm(_), Address::Evm(_))
            )
    }

    pub async fn quote(&self, request: &Request, now: u64) -> Result<(Route, Order), SwapError> {
        let (Some(source), Some(target), Some(Address::Evm(token)), Address::Evm(to)) = (
            self.domains
                .get(&request.sell)
                .filter(|_| self.covers(request)),
            self.domains.get(&request.buy),
            request.sell.contract(),
            request.to,
        ) else {
            return Err(SwapError::Unsupported);
        };
        let units = request.amount.units();
        let fees = self.fees(*source, *target, units).await?;
        let received = |taken: U256| units.checked_sub(taken).filter(|left| !left.is_zero());
        let (Some(expected), Some(minimum)) = (received(fees.expected), received(fees.max)) else {
            return Err(SwapError::Refused(
                "the amount is too small for Circle's fees".to_owned(),
            ));
        };
        let data = deposit_data(units, *target, &to, token, fees.max);
        let amount =
            |units: U256| Amount::new(units, request.buy_decimals).map_err(|_| unreadable());
        let charged = |kind, units: U256| -> Result<Cost, SwapError> {
            Ok(Cost {
                kind,
                asset: request.sell.clone(),
                amount: Amount::new(units, request.amount.decimals()).map_err(|_| unreadable())?,
                charged: Charged::InQuote,
            })
        };
        let forwarding = fees
            .expected
            .checked_sub(fees.fast)
            .ok_or_else(unreadable)?;
        let costs = [
            (CostKind::ProviderFee, fees.fast),
            (CostKind::NetworkFee, forwarding),
        ]
        .into_iter()
        .filter(|(_, units)| !units.is_zero())
        .map(|(kind, units)| charged(kind, units))
        .collect::<Result<Vec<_>, _>>()?;
        let route = Route {
            provider: Provider::Circle.name(),
            method: Method::Issuer,
            sell: request.sell.clone(),
            amount: request.amount,
            buy: request.buy.clone(),
            expected: amount(expected)?,
            minimum: amount(minimum)?,
            costs,
            wait: Some(Wait::About(FAST_SECONDS)),
        };
        let messenger = Address::Evm(self.messenger);
        let order = Order {
            provider: Provider::Circle,
            reference: String::new(),
            deposit: messenger,
            comment: None,
            amount: request.amount,
            expected: route.expected,
            minimum: route.minimum,
            expires_at: now.saturating_add(QUOTE_LIFE),
            contract: Some(Contract {
                call: Call {
                    to: messenger,
                    value: Amount::new(U256::ZERO, request.native_decimals)
                        .map_err(|_| unreadable())?,
                    data,
                },
                spender: Some(messenger),
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
        let domain = self
            .domains
            .iter()
            .find(|(asset, _)| asset.network() == network)
            .map(|(_, domain)| *domain)
            .ok_or(SwapError::Unsupported)?;
        let valid = transaction.len() == 66
            && transaction
                .strip_prefix("0x")
                .is_some_and(|digits| bahaa_types::hex::decode(digits).is_some());
        if !valid {
            return Err(unreadable());
        }
        let mut url = self
            .base
            .join(&format!("v2/messages/{domain}"))
            .map_err(|_| unreadable())?;
        url.query_pairs_mut()
            .append_pair("transactionHash", transaction);
        progress(&fetch(self.client.get(url)).await?)
    }

    async fn fees(&self, source: u32, target: u32, units: U256) -> Result<Fees, SwapError> {
        let mut url = self
            .base
            .join(&format!("v2/burn/USDC/fees/{source}/{target}"))
            .map_err(|_| unreadable())?;
        url.query_pairs_mut().append_pair("forward", "true");
        let answer = fetch(self.client.get(url)).await?;
        let fast = answer
            .as_array()
            .and_then(|tiers| tiers.iter().find(|tier| tier["finalityThreshold"] == FAST))
            .ok_or(SwapError::Refused(
                "Circle offers no fast transfer here".to_owned(),
            ))?;
        let bps = fast["minimumFee"]
            .as_number()
            .and_then(|number| Amount::parse(&number.to_string(), 2).ok())
            .map(|bps| bps.units())
            .ok_or_else(unreadable)?;
        let forward = |level: &str| -> Result<U256, SwapError> {
            fast["forwardFee"][level]
                .as_u64()
                .map(U256::from)
                .ok_or_else(unreadable)
        };
        let protocol = units
            .checked_mul(bps)
            .map(|scaled| scaled.div_ceil(U256::from(HUNDREDTH_BPS)))
            .ok_or_else(unreadable)?;
        let total = |forward: U256| protocol.checked_add(forward).ok_or_else(unreadable);
        Ok(Fees {
            fast: protocol,
            expected: total(forward("med")?)?,
            max: total(forward("high")?)?,
        })
    }
}

fn deposit_data(
    units: U256,
    domain: u32,
    to: &[u8; 20],
    token: &[u8; 20],
    max_fee: U256,
) -> Vec<u8> {
    let mut data = DEPOSIT.to_vec();
    abi::push_uint(&mut data, units);
    abi::push_uint(&mut data, U256::from(domain));
    abi::push_address(&mut data, to);
    abi::push_address(&mut data, token);
    abi::push_uint(&mut data, U256::ZERO);
    abi::push_uint(&mut data, max_fee);
    abi::push_uint(&mut data, U256::from(FAST));
    abi::push_uint(&mut data, U256::from(256u16));
    abi::push_uint(&mut data, U256::from(32u8));
    let mut hook = [0u8; 32];
    hook[..FORWARD.len()].copy_from_slice(&FORWARD);
    data.extend_from_slice(&hook);
    data
}

fn progress(answer: &Value) -> Result<Progress, SwapError> {
    let [message] = answer["messages"]
        .as_array()
        .map(Vec::as_slice)
        .ok_or_else(unreadable)?
    else {
        return Err(unreadable());
    };
    let forward = message["forwardState"].as_str().unwrap_or_default();
    let minted = message["destinationMintTxHash"]
        .as_str()
        .is_some_and(|hash| !hash.is_empty());
    Ok(match (message["status"].as_str(), forward) {
        (_, "FAILED") => Progress::Failed(
            "Circle could not mint on the destination, the USDC is waiting there to be claimed"
                .to_owned(),
        ),
        (_, "COMPLETE") => Progress::Done,
        _ if minted => Progress::Done,
        (Some("complete"), _) => Progress::Swapping,
        (Some("pending_confirmations"), _) => Progress::Received,
        _ => return Err(unreadable()),
    })
}

#[cfg(fuzzing)]
pub(crate) fn fuzz(value: &Value) {
    let _ = progress(value);
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use sha3::{Digest, Keccak256};

    use super::*;

    #[test]
    fn the_burn_is_deposit_for_burn_with_the_forwarding_hook() {
        let selector = Keccak256::digest(
            b"depositForBurnWithHook(uint256,uint32,bytes32,address,bytes32,uint256,uint32,bytes)",
        );
        assert_eq!(selector[..4], DEPOSIT);
        let data = deposit_data(U256::from(5u8), 6, &[1; 20], &[2; 20], U256::from(3u8));
        assert_eq!(data.len(), 4 + 10 * 32);
        assert_eq!(
            bahaa_types::hex::encode(&data[data.len() - 32..]),
            "636374702d666f72776172640000000000000000000000000000000000000000"
        );
        assert_eq!(abi::uint(&data[4..], 6), Some(U256::from(FAST)));
    }

    #[test]
    fn message_states_map_to_progress() {
        let message = |status: &str, forward: &str| json!({"messages": [{"status": status, "forwardState": forward}]});
        assert_eq!(
            progress(&message("pending_confirmations", "")).unwrap(),
            Progress::Received
        );
        assert_eq!(
            progress(&message("complete", "PENDING")).unwrap(),
            Progress::Swapping
        );
        assert_eq!(
            progress(&message("complete", "COMPLETE")).unwrap(),
            Progress::Done
        );
        assert!(matches!(
            progress(&message("complete", "FAILED")),
            Ok(Progress::Failed(_))
        ));
        assert!(progress(&json!({"messages": []})).is_err());
    }
}
