use std::collections::BTreeMap;

use bahaa_chain::{Call, Wait};
use bahaa_types::{Address, Amount, AssetId, ChainFamily, NetworkId};
use reqwest::Client;
use ruint::aliases::U256;
use serde_json::{Value, json};
use url::Url;

use crate::config::{ChainflipAsset, ChainflipConfig};
use crate::order::{Contract, Order, Progress, Provider, QUOTE_LIFE, Request, SwapError};
use crate::reply::{fetch, unreadable};
use crate::route::{Charged, Cost, CostKind, Method, Route};

const VERSION: u8 = 1;
const SLIPPAGE_BPS: u32 = 100;
const WHOLE_BPS: u32 = 10_000;
const BLOCKS_PER_MINUTE: u64 = 10;
const RETRY_BLOCKS: u16 = 100;
const MAX_RETRY_BLOCKS: u16 = 600;
const NO_ORACLE_LIMIT: u8 = u8::MAX;
const CHUNK_INTERVAL: u16 = 2;
const MAX_PAYLOAD: usize = 80;

pub struct Chainflip {
    client: Client,
    base: Url,
    rpc: Url,
    network: NetworkId,
    assets: BTreeMap<AssetId, ChainflipAsset>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Payload {
    asset: u8,
    address: Vec<u8>,
    retry: u16,
    minimum: u128,
    oracle: u8,
    chunks: u16,
    interval: u16,
    boost: u8,
    broker: u8,
    affiliates: u8,
}

impl Payload {
    fn encode(&self) -> Vec<u8> {
        let mut data = vec![VERSION, self.asset];
        data.extend_from_slice(&self.address);
        data.extend_from_slice(&self.retry.to_le_bytes());
        data.extend_from_slice(&self.minimum.to_le_bytes());
        data.push(self.oracle);
        data.extend_from_slice(&self.chunks.to_le_bytes());
        data.extend_from_slice(&self.interval.to_le_bytes());
        data.extend_from_slice(&[self.boost, self.broker, self.affiliates]);
        data
    }

    fn decode(data: &[u8], width: usize) -> Option<Self> {
        let (&VERSION, rest) = data.split_first()? else {
            return None;
        };
        let (&asset, rest) = rest.split_first()?;
        let (address, rest) = rest.split_at_checked(width)?;
        let (retry, rest) = rest.split_first_chunk::<2>()?;
        let (minimum, rest) = rest.split_first_chunk::<16>()?;
        let (&oracle, rest) = rest.split_first()?;
        let (chunks, rest) = rest.split_first_chunk::<2>()?;
        let (interval, rest) = rest.split_first_chunk::<2>()?;
        let [boost, broker, affiliates] = *rest else {
            return None;
        };
        Some(Self {
            asset,
            address: address.to_vec(),
            retry: u16::from_le_bytes(*retry),
            minimum: u128::from_le_bytes(*minimum),
            oracle,
            chunks: u16::from_le_bytes(*chunks),
            interval: u16::from_le_bytes(*interval),
            boost,
            broker,
            affiliates,
        })
    }
}

struct Quoted {
    egress: U256,
    retry: u16,
    oracle: u8,
    seconds: Option<u64>,
    fees: Vec<(String, String, String, U256)>,
}

impl Chainflip {
    pub fn new(config: &ChainflipConfig, client: Client) -> Self {
        Self {
            client,
            base: config.url.clone(),
            rpc: config.rpc.clone(),
            network: config.network.clone(),
            assets: config.assets.clone(),
        }
    }

    pub fn covers(&self, request: &Request) -> bool {
        let fits = match (self.assets.get(&request.buy), request.to) {
            (Some(asset), to) => destination(asset, &to).is_some(),
            (None, _) => false,
        };
        request.sell == AssetId::Native(self.network.clone())
            && request.from.family() == ChainFamily::Bitcoin
            && request.buy.network() != &self.network
            && fits
    }

    pub async fn quote(&self, request: &Request, now: u64) -> Result<(Route, Order), SwapError> {
        if !self.covers(request) {
            return Err(SwapError::Unsupported);
        }
        let (Some(sold), Some(bought)) = (
            self.assets.get(&request.sell),
            self.assets.get(&request.buy),
        ) else {
            return Err(SwapError::Unsupported);
        };
        let units = request.amount.units();
        let quoted = self.quoted(sold, bought, units).await?;
        let minimum = quoted
            .egress
            .checked_mul(U256::from(WHOLE_BPS - SLIPPAGE_BPS))
            .map(|scaled| scaled / U256::from(WHOLE_BPS))
            .and_then(|minimum| u128::try_from(minimum).ok())
            .ok_or_else(unreadable)?;
        let address = destination(bought, &request.to).ok_or(SwapError::Unsupported)?;
        let ours = Payload {
            asset: bought.id,
            address,
            retry: quoted.retry,
            minimum,
            oracle: quoted.oracle,
            chunks: 1,
            interval: CHUNK_INTERVAL,
            boost: 0,
            broker: 0,
            affiliates: 0,
        };
        let vault = self.vault(request, sold, bought, &ours).await?;
        let data = ours.encode();
        if data.len() > MAX_PAYLOAD {
            return Err(SwapError::Unsupported);
        }
        let amount =
            |units: U256, decimals: u8| Amount::new(units, decimals).map_err(|_| unreadable());
        let expected = amount(quoted.egress, request.buy_decimals)?;
        let lowest = amount(U256::from(minimum), request.buy_decimals)?;
        let route = Route {
            provider: Provider::Chainflip.name(),
            method: Method::Validators,
            sell: request.sell.clone(),
            amount: request.amount,
            buy: request.buy.clone(),
            expected,
            minimum: lowest,
            costs: self.costs(&quoted.fees),
            wait: quoted.seconds.map(Wait::About),
        };
        let order = Order {
            provider: Provider::Chainflip,
            reference: String::new(),
            deposit: vault,
            comment: None,
            amount: request.amount,
            expected,
            minimum: lowest,
            expires_at: now.saturating_add(QUOTE_LIFE),
            contract: Some(Contract {
                call: Call {
                    to: vault,
                    value: request.amount,
                    data,
                },
                spender: None,
                ours: true,
                spent: None,
            }),
            typed: None,
        };
        Ok((route, order))
    }

    pub async fn status(&self, transaction: &str) -> Result<Progress, SwapError> {
        let valid = transaction.len() == 64 && bahaa_types::hex::decode(transaction).is_some();
        if !valid {
            return Err(unreadable());
        }
        let url = self
            .base
            .join(&format!("v2/swaps/{transaction}"))
            .map_err(|_| unreadable())?;
        let answer = fetch(self.client.get(url)).await?;
        let sent = |field: &str| answer.get(field).is_some_and(|value| !value.is_null());
        match answer["state"].as_str() {
            Some("WAITING") => Ok(Progress::Waiting),
            Some("RECEIVING") => Ok(Progress::Received),
            Some("SWAPPING" | "SENDING" | "SENT") => Ok(Progress::Swapping),
            Some("COMPLETED") if sent("swapEgress") => Ok(Progress::Done),
            Some("COMPLETED") if sent("refundEgress") => Ok(Progress::Refunded),
            Some("FAILED") => Ok(Progress::Failed(
                "Chainflip could not finish the swap".to_owned(),
            )),
            _ => Err(unreadable()),
        }
    }

    async fn quoted(
        &self,
        sold: &ChainflipAsset,
        bought: &ChainflipAsset,
        units: U256,
    ) -> Result<Quoted, SwapError> {
        let mut url = self.base.join("v2/quote").map_err(|_| unreadable())?;
        url.query_pairs_mut()
            .append_pair("amount", &units.to_string())
            .append_pair("srcChain", &sold.chain)
            .append_pair("srcAsset", &sold.asset)
            .append_pair("destChain", &bought.chain)
            .append_pair("destAsset", &bought.asset)
            .append_pair("isVaultSwap", "true")
            .append_pair("isOnChain", "false");
        let quotes = fetch(self.client.get(url)).await?;
        let quote = quotes
            .as_array()
            .and_then(|quotes| quotes.iter().find(|quote| quote["type"] == "REGULAR"))
            .ok_or(SwapError::Refused(
                "Chainflip has no quote for this swap".to_owned(),
            ))?;
        let same = |field: &str, asset: &ChainflipAsset| {
            quote[field]["chain"] == asset.chain.as_str()
                && quote[field]["asset"] == asset.asset.as_str()
        };
        let number = |value: &Value| -> Option<U256> { value.as_str()?.parse().ok() };
        if !same("srcAsset", sold)
            || !same("destAsset", bought)
            || number(&quote["depositAmount"]) != Some(units)
        {
            return Err(SwapError::Untrusted("the quote is for another request"));
        }
        let egress = number(&quote["egressAmount"])
            .filter(|egress| !egress.is_zero())
            .ok_or_else(unreadable)?;
        let retry = quote["recommendedRetryDurationMinutes"]
            .as_u64()
            .and_then(|minutes| minutes.checked_mul(BLOCKS_PER_MINUTE))
            .and_then(|blocks| u16::try_from(blocks).ok())
            .map_or(RETRY_BLOCKS, |blocks| {
                blocks.clamp(RETRY_BLOCKS, MAX_RETRY_BLOCKS)
            });
        let oracle = quote
            .get("recommendedLivePriceSlippageTolerancePercent")
            .filter(|value| value.is_number())
            .and_then(|value| Amount::parse(&value.to_string(), 2).ok())
            .and_then(|bps| u8::try_from(bps.units()).ok())
            .filter(|bps| *bps < NO_ORACLE_LIMIT)
            .unwrap_or(NO_ORACLE_LIMIT);
        let fees = quote["includedFees"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|fee| {
                Some((
                    fee["type"].as_str()?.to_owned(),
                    fee["chain"].as_str()?.to_owned(),
                    fee["asset"].as_str()?.to_owned(),
                    number(&fee["amount"])?,
                ))
            })
            .collect();
        Ok(Quoted {
            egress,
            retry,
            oracle,
            seconds: quote["estimatedDurationSeconds"].as_u64(),
            fees,
        })
    }

    async fn vault(
        &self,
        request: &Request,
        sold: &ChainflipAsset,
        bought: &ChainflipAsset,
        ours: &Payload,
    ) -> Result<Address, SwapError> {
        let refund = request.from.to_string();
        let numerator: U256 = U256::from(ours.minimum) << 128;
        let price = numerator.div_ceil(request.amount.units());
        let body = json!({
            "srcAsset": {"chain": sold.chain, "asset": sold.asset},
            "destAsset": {"chain": bought.chain, "asset": bought.asset},
            "srcAddress": refund,
            "destAddress": request.to.to_string(),
            "amount": request.amount.units().to_string(),
            "commissionBps": 0,
            "fillOrKillParams": {
                "retryDurationBlocks": ours.retry,
                "refundAddress": refund,
                "minPriceX128": price.to_string(),
                "maxOraclePriceSlippage": (ours.oracle != NO_ORACLE_LIMIT).then_some(ours.oracle),
            },
        });
        let url = self
            .base
            .join("api/encodeVaultSwapData")
            .map_err(|_| unreadable())?;
        let encoded = fetch(self.client.post(url).json(&body)).await?;
        let text = |field: &str| encoded[field].as_str().ok_or_else(unreadable);
        if text("chain")? != sold.chain {
            return Err(SwapError::Untrusted(
                "Chainflip answered for another network",
            ));
        }
        let theirs = text("nulldataPayload")?
            .strip_prefix("0x")
            .and_then(bahaa_types::hex::decode)
            .and_then(|data| Payload::decode(&data, ours.address.len()))
            .ok_or(SwapError::Untrusted("Chainflip's swap data can't be read"))?;
        let agreed = theirs.asset == ours.asset
            && theirs.address == ours.address
            && theirs.broker == 0
            && theirs.affiliates == 0
            && theirs.boost == 0;
        if !agreed {
            return Err(SwapError::Untrusted(
                "Chainflip's swap data pays someone else or takes a fee",
            ));
        }
        let deposit = text("depositAddress")?;
        if !self.listed(deposit).await? {
            return Err(SwapError::Untrusted(
                "the deposit address is not a Chainflip vault",
            ));
        }
        let vault = Address::parse(ChainFamily::Bitcoin, deposit).map_err(|_| unreadable())?;
        if vault == request.from {
            return Err(SwapError::Untrusted(
                "the deposit address is this wallet's own",
            ));
        }
        Ok(vault)
    }

    async fn listed(&self, deposit: &str) -> Result<bool, SwapError> {
        let body =
            json!({"jsonrpc": "2.0", "id": 1, "method": "cf_get_vault_addresses", "params": []});
        let reply = fetch(self.client.post(self.rpc.clone()).json(&body)).await?;
        let vaults = reply["result"]["bitcoin"]
            .as_array()
            .ok_or_else(unreadable)?;
        Ok(vaults.iter().any(|entry| {
            entry[1]["Btc"]
                .as_array()
                .and_then(|bytes| {
                    bytes
                        .iter()
                        .map(|byte| byte.as_u64().and_then(|byte| u8::try_from(byte).ok()))
                        .collect::<Option<Vec<u8>>>()
                })
                .is_some_and(|bytes| bytes == deposit.as_bytes())
        }))
    }

    fn costs(&self, fees: &[(String, String, String, U256)]) -> Vec<Cost> {
        fees.iter()
            .filter(|(_, _, _, units)| !units.is_zero())
            .filter_map(|(kind, chain, asset, units)| {
                let (id, known) = self
                    .assets
                    .iter()
                    .find(|(_, known)| known.chain == *chain && known.asset == *asset)?;
                let kind = match kind.as_str() {
                    "INGRESS" | "EGRESS" => CostKind::NetworkFee,
                    _ => CostKind::ProviderFee,
                };
                Some(Cost {
                    kind,
                    asset: id.clone(),
                    amount: Amount::new(*units, known.decimals).ok()?,
                    charged: Charged::InQuote,
                })
            })
            .collect()
    }
}

fn destination(asset: &ChainflipAsset, to: &Address) -> Option<Vec<u8>> {
    match (asset.chain.as_str(), to) {
        ("Ethereum" | "Arbitrum", Address::Evm(bytes)) | ("Tron", Address::Tron(bytes)) => {
            Some(bytes.to_vec())
        }
        ("Solana", Address::Solana(bytes)) => Some(bytes.to_vec()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RECORDED: &str = "01039858effd232b4033e47d90003d41ec34ecaeda94640033621d090000000000000000000000003201000200000000";

    #[test]
    fn our_payload_matches_the_one_chainflip_encoded() {
        let address = bahaa_types::hex::decode("9858effd232b4033e47d90003d41ec34ecaeda94").unwrap();
        let ours = Payload {
            asset: 3,
            address,
            retry: 100,
            minimum: 152_920_627,
            oracle: 50,
            chunks: 1,
            interval: CHUNK_INTERVAL,
            boost: 0,
            broker: 0,
            affiliates: 0,
        };
        assert_eq!(bahaa_types::hex::encode(&ours.encode()), RECORDED);
        let read = Payload::decode(&bahaa_types::hex::decode(RECORDED).unwrap(), 20).unwrap();
        assert_eq!(read, ours);
        let mut fee = bahaa_types::hex::decode(RECORDED).unwrap();
        let broker = fee.len() - 2;
        fee[broker] = 25;
        assert_eq!(Payload::decode(&fee, 20).unwrap().broker, 25);
        assert_eq!(Payload::decode(&fee[..fee.len() - 1], 20), None);
    }
}
