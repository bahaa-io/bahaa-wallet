use bahaa_chain::{Call, Chain, Wait};
use bahaa_solana::{SolanaChain, swap_instruction};
use bahaa_types::{Address, Amount, AssetId, ChainFamily, NetworkId};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use reqwest::Client;
use ruint::aliases::U256;
use serde_json::json;
use url::Url;

use crate::config::JupiterConfig;
use crate::order::{Contract, Order, Provider, QUOTE_LIFE, Request, SwapError};
use crate::reply::{fetch, unreadable};
use crate::route::{Method, Route};

const NATIVE: &str = "So11111111111111111111111111111111111111112";
const ROUTE: [u8; 8] = [0xe5, 0x17, 0xcb, 0x97, 0x7a, 0xe3, 0xad, 0x2a];
const SHARED_ROUTE: [u8; 8] = [0xc1, 0x20, 0x9b, 0x33, 0x41, 0xd6, 0x9c, 0x81];
const TAIL: usize = 19;
const SLIPPAGE_BPS: u16 = 50;
const WHOLE_BPS: u16 = 10_000;
const CONFIRM_SLOTS: u64 = 4;

pub struct Jupiter {
    client: Client,
    base: Url,
    network: NetworkId,
    program: [u8; 32],
}

#[derive(Debug, PartialEq, Eq)]
struct Terms {
    sold: u64,
    quoted: u64,
    slippage: u16,
    fee_bps: u8,
}

impl Jupiter {
    pub fn new(config: &JupiterConfig, client: Client) -> Result<Self, SwapError> {
        let Ok(Address::Solana(program)) = Address::parse(ChainFamily::Solana, &config.program)
        else {
            return Err(SwapError::Untrusted(
                "the built-in Jupiter program is broken",
            ));
        };
        Ok(Self {
            client,
            base: config.url.clone(),
            network: config.network.clone(),
            program,
        })
    }

    pub fn covers(&self, request: &Request) -> bool {
        *request.sell.network() == self.network
            && *request.buy.network() == self.network
            && request.sell != request.buy
            && matches!(
                (request.from, request.to),
                (Address::Solana(_), Address::Solana(_))
            )
    }

    pub async fn quote(
        &self,
        chain: &SolanaChain,
        request: &Request,
        now: u64,
    ) -> Result<(Route, Order), SwapError> {
        if !self.covers(request) {
            return Err(SwapError::Unsupported);
        }
        let (sold, bought) = (mint(&request.sell), mint(&request.buy));
        let units = u64::try_from(request.amount.units()).map_err(|_| SwapError::Unsupported)?;
        let mut url = self.base.join("quote").map_err(|_| unreadable())?;
        url.query_pairs_mut()
            .append_pair("inputMint", &sold)
            .append_pair("outputMint", &bought)
            .append_pair("amount", &units.to_string())
            .append_pair("slippageBps", &SLIPPAGE_BPS.to_string())
            .append_pair("swapMode", "ExactIn")
            .append_pair("restrictIntermediateTokens", "true");
        let quote = fetch(self.client.get(url)).await?;
        let text = |field: &str| quote[field].as_str().ok_or_else(unreadable);
        let honest = text("inputMint")? == sold
            && text("outputMint")? == bought
            && text("inAmount")? == units.to_string()
            && text("swapMode")? == "ExactIn"
            && quote["platformFee"].is_null();
        if !honest {
            return Err(SwapError::Untrusted("the quote is for another request"));
        }
        let out: u64 = text("outAmount")?.parse().map_err(|_| unreadable())?;
        let Address::Solana(owner) = request.from else {
            return Err(SwapError::Unsupported);
        };
        let swap = self.base.join("swap").map_err(|_| unreadable())?;
        let built = fetch(self.client.post(swap).json(&json!({
            "quoteResponse": quote,
            "userPublicKey": bs58::encode(owner).into_string(),
            "wrapAndUnwrapSol": true,
            "dynamicComputeUnitLimit": true,
        })))
        .await?;
        let transaction = built["swapTransaction"]
            .as_str()
            .and_then(|text| STANDARD.decode(text).ok())
            .ok_or_else(unreadable)?;
        let terms = swap_instruction(&transaction)
            .filter(|(program, _)| *program == self.program)
            .and_then(|(_, data)| terms(&data))
            .ok_or(SwapError::Untrusted(
                "the route asks for more than a plain swap",
            ))?;
        if terms.sold != units
            || terms.quoted != out
            || terms.slippage > SLIPPAGE_BPS
            || terms.fee_bps != 0
        {
            return Err(SwapError::Untrusted(
                "the route pays someone else or less than it shows",
            ));
        }
        let guaranteed =
            u128::from(out) * u128::from(WHOLE_BPS - terms.slippage) / u128::from(WHOLE_BPS);
        let amount =
            |units: U256| Amount::new(units, request.buy_decimals).map_err(|_| unreadable());
        let (expected, minimum) = (amount(U256::from(out))?, amount(U256::from(guaranteed))?);
        let route = Route {
            provider: Provider::Jupiter.name(),
            method: Method::Aggregator,
            sell: request.sell.clone(),
            amount: request.amount,
            buy: request.buy.clone(),
            expected,
            minimum,
            costs: Vec::new(),
            wait: Some(Wait::blocks(CONFIRM_SLOTS, chain.network().block_ms)),
        };
        let program = Address::Solana(self.program);
        let order = Order {
            provider: Provider::Jupiter,
            reference: String::new(),
            deposit: program,
            comment: None,
            amount: request.amount,
            expected,
            minimum,
            expires_at: now.saturating_add(QUOTE_LIFE),
            contract: Some(Contract {
                call: Call {
                    to: program,
                    value: Amount::new(U256::ZERO, request.native_decimals)
                        .map_err(|_| unreadable())?,
                    data: transaction,
                },
                spender: None,
                ours: false,
                spent: None,
            }),
            typed: None,
        };
        Ok((route, order))
    }
}

fn mint(asset: &AssetId) -> String {
    asset
        .contract()
        .map_or_else(|| NATIVE.to_owned(), ToString::to_string)
}

fn terms(data: &[u8]) -> Option<Terms> {
    let kind: [u8; 8] = data.get(..8)?.try_into().ok()?;
    if kind != ROUTE && kind != SHARED_ROUTE {
        return None;
    }
    let tail = data.get(data.len().checked_sub(TAIL)?..)?;
    Some(Terms {
        sold: u64::from_le_bytes(tail[..8].try_into().ok()?),
        quoted: u64::from_le_bytes(tail[8..16].try_into().ok()?),
        slippage: u16::from_le_bytes(tail[16..18].try_into().ok()?),
        fee_bps: tail[18],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_route_instruction_ends_with_its_amounts_slippage_and_fee() {
        let mut data = ROUTE.to_vec();
        data.extend_from_slice(&[7; 9]);
        data.extend_from_slice(&100_000_000u64.to_le_bytes());
        data.extend_from_slice(&9_331_279u64.to_le_bytes());
        data.extend_from_slice(&50u16.to_le_bytes());
        data.push(0);
        assert_eq!(
            terms(&data),
            Some(Terms {
                sold: 100_000_000,
                quoted: 9_331_279,
                slippage: 50,
                fee_bps: 0,
            })
        );
        data[0] = 0xd0;
        assert_eq!(terms(&data), None);
    }
}
