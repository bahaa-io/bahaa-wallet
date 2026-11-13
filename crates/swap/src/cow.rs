use std::collections::BTreeMap;

use bahaa_chain::Wait;
use bahaa_types::{Address, Amount, AssetId, ChainFamily, NetworkId};
use reqwest::Client;
use ruint::aliases::U256;
use serde_json::{Value, json};
use sha3::{Digest, Keccak256};
use url::Url;

use crate::config::{CowConfig, CowNetwork};
use crate::order::{Order, Progress, Provider, QUOTE_LIFE, Request, SwapError};
use crate::reply::{fetch, unreadable};
use crate::route::{Charged, Cost, CostKind, Method, Route};
use bahaa_chain::abi;

const NATIVE: [u8; 20] = [0xee; 20];
const APP_DATA: &str = "{}";
const DOMAIN_TYPE: &str =
    "EIP712Domain(string name,string version,uint256 chainId,address verifyingContract)";
const ORDER_TYPE: &str = "Order(address sellToken,address buyToken,address receiver,uint256 sellAmount,uint256 buyAmount,uint32 validTo,bytes32 appData,uint256 feeAmount,string kind,bool partiallyFillable,string sellTokenBalance,string buyTokenBalance)";
const SLIPPAGE_BPS: u32 = 50;
const WHOLE_BPS: u32 = 10_000;
const ORDER_LIFE: u64 = 30 * 60;
const BATCH_SECONDS: u64 = 60;
const UID_LEN: usize = 56;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TypedOrder {
    pub chain: u64,
    pub settlement: [u8; 20],
    pub owner: [u8; 20],
    pub sell: [u8; 20],
    pub buy: [u8; 20],
    pub receiver: [u8; 20],
    pub sell_amount: U256,
    pub buy_amount: U256,
    pub valid_to: u32,
    pub quote: Option<i64>,
}

impl TypedOrder {
    pub fn digest(&self) -> [u8; 32] {
        let mut domain = Vec::with_capacity(5 * 32);
        domain.extend_from_slice(&keccak(DOMAIN_TYPE.as_bytes()));
        domain.extend_from_slice(&keccak(b"Gnosis Protocol"));
        domain.extend_from_slice(&keccak(b"v2"));
        abi::push_uint(&mut domain, U256::from(self.chain));
        abi::push_address(&mut domain, &self.settlement);
        let mut order = Vec::with_capacity(13 * 32);
        order.extend_from_slice(&keccak(ORDER_TYPE.as_bytes()));
        abi::push_address(&mut order, &self.sell);
        abi::push_address(&mut order, &self.buy);
        abi::push_address(&mut order, &self.receiver);
        abi::push_uint(&mut order, self.sell_amount);
        abi::push_uint(&mut order, self.buy_amount);
        abi::push_uint(&mut order, U256::from(self.valid_to));
        order.extend_from_slice(&keccak(APP_DATA.as_bytes()));
        abi::push_uint(&mut order, U256::ZERO);
        order.extend_from_slice(&keccak(b"sell"));
        abi::push_uint(&mut order, U256::ZERO);
        order.extend_from_slice(&keccak(b"erc20"));
        order.extend_from_slice(&keccak(b"erc20"));
        let mut message = vec![0x19, 0x01];
        message.extend_from_slice(&keccak(&domain));
        message.extend_from_slice(&keccak(&order));
        keccak(&message)
    }

    pub fn uid(&self) -> String {
        let mut uid = self.digest().to_vec();
        uid.extend_from_slice(&self.owner);
        uid.extend_from_slice(&self.valid_to.to_be_bytes());
        format!("0x{}", bahaa_types::hex::encode(&uid))
    }

    pub fn body(&self, signature: &[u8; 65]) -> Value {
        let hex = |bytes: &[u8]| format!("0x{}", bahaa_types::hex::encode(bytes));
        let mut body = json!({
            "sellToken": hex(&self.sell),
            "buyToken": hex(&self.buy),
            "receiver": hex(&self.receiver),
            "sellAmount": self.sell_amount.to_string(),
            "buyAmount": self.buy_amount.to_string(),
            "validTo": self.valid_to,
            "appData": APP_DATA,
            "feeAmount": "0",
            "kind": "sell",
            "partiallyFillable": false,
            "sellTokenBalance": "erc20",
            "buyTokenBalance": "erc20",
            "signingScheme": "eip712",
            "signature": hex(signature),
            "from": hex(&self.owner),
        });
        if let Some(quote) = self.quote {
            body["quoteId"] = json!(quote);
        }
        body
    }
}

pub struct Cow {
    client: Client,
    base: Url,
    settlement: [u8; 20],
    relayer: [u8; 20],
    networks: BTreeMap<NetworkId, CowNetwork>,
}

impl Cow {
    pub fn new(config: &CowConfig, client: Client) -> Result<Self, SwapError> {
        let evm = |text: &str| match Address::parse(ChainFamily::Evm, text) {
            Ok(Address::Evm(bytes)) => Ok(bytes),
            _ => Err(SwapError::Untrusted(
                "the built-in CoW contracts are broken",
            )),
        };
        Ok(Self {
            client,
            base: config.url.clone(),
            settlement: evm(&config.settlement)?,
            relayer: evm(&config.relayer)?,
            networks: config.networks.clone(),
        })
    }

    pub fn relayer(&self) -> Address {
        Address::Evm(self.relayer)
    }

    pub fn covers(&self, request: &Request) -> bool {
        request.sell.network() == request.buy.network()
            && request.sell != request.buy
            && request.sell.contract().is_some()
            && self.networks.contains_key(request.sell.network())
            && matches!(
                (request.from, request.to),
                (Address::Evm(_), Address::Evm(_))
            )
    }

    pub async fn quote(&self, request: &Request, now: u64) -> Result<(Route, Order), SwapError> {
        let network = self
            .networks
            .get(request.sell.network())
            .filter(|_| self.covers(request))
            .ok_or(SwapError::Unsupported)?;
        let (Address::Evm(owner), Address::Evm(receiver)) = (request.from, request.to) else {
            return Err(SwapError::Unsupported);
        };
        let (paid, bought_token) = (token(&request.sell)?, token(&request.buy)?);
        let units = request.amount.units();
        let hex = |bytes: &[u8; 20]| format!("0x{}", bahaa_types::hex::encode(bytes));
        let url = self.url(&network.path, "quote")?;
        let answer = fetch(self.client.post(url).json(&json!({
            "sellToken": hex(&paid),
            "buyToken": hex(&bought_token),
            "from": hex(&owner),
            "receiver": hex(&receiver),
            "sellAmountBeforeFee": units.to_string(),
            "kind": "sell",
            "partiallyFillable": false,
            "sellTokenBalance": "erc20",
            "buyTokenBalance": "erc20",
            "signingScheme": "eip712",
            "onchainOrder": false,
            "priceQuality": "verified",
            "appData": APP_DATA,
        })))
        .await?;
        let quote = &answer["quote"];
        let number = |field: &str| -> Result<U256, SwapError> {
            quote[field]
                .as_str()
                .and_then(|text| text.parse().ok())
                .ok_or_else(unreadable)
        };
        let same = |field: &str, wanted: &[u8; 20]| {
            quote[field]
                .as_str()
                .is_some_and(|text| text.eq_ignore_ascii_case(&hex(wanted)))
        };
        let (sold, fee, bought) = (
            number("sellAmount")?,
            number("feeAmount")?,
            number("buyAmount")?,
        );
        let honest = same("sellToken", &paid)
            && same("buyToken", &bought_token)
            && same("receiver", &receiver)
            && quote["kind"] == "sell"
            && sold.checked_add(fee) == Some(units)
            && !bought.is_zero();
        if !honest {
            return Err(SwapError::Untrusted("the quote is for another request"));
        }
        let minimum = bought
            .checked_mul(U256::from(WHOLE_BPS - SLIPPAGE_BPS))
            .map(|scaled| scaled / U256::from(WHOLE_BPS))
            .ok_or_else(unreadable)?;
        let typed = TypedOrder {
            chain: network.chain,
            settlement: self.settlement,
            owner,
            sell: paid,
            buy: bought_token,
            receiver,
            sell_amount: units,
            buy_amount: minimum,
            valid_to: u32::try_from(now.saturating_add(ORDER_LIFE)).map_err(|_| unreadable())?,
            quote: answer["id"].as_i64(),
        };
        Self::offer(request, typed, (bought, fee), now, self.relayer())
    }

    fn offer(
        request: &Request,
        typed: TypedOrder,
        (bought, fee): (U256, U256),
        now: u64,
        relayer: Address,
    ) -> Result<(Route, Order), SwapError> {
        let minimum = typed.buy_amount;
        let bought_amount =
            |units: U256| Amount::new(units, request.buy_decimals).map_err(|_| unreadable());
        let (expected, minimum) = (bought_amount(bought)?, bought_amount(minimum)?);
        let route = Route {
            provider: Provider::Cow.name(),
            method: Method::Auction,
            sell: request.sell.clone(),
            amount: request.amount,
            buy: request.buy.clone(),
            expected,
            minimum,
            costs: vec![Cost {
                kind: CostKind::NetworkFee,
                asset: request.sell.clone(),
                amount: Amount::new(fee, request.amount.decimals()).map_err(|_| unreadable())?,
                charged: Charged::InQuote,
            }],
            wait: Some(Wait::About(BATCH_SECONDS)),
        };
        let order = Order {
            provider: Provider::Cow,
            reference: typed.uid(),
            deposit: relayer,
            comment: None,
            amount: request.amount,
            expected,
            minimum,
            expires_at: now.saturating_add(QUOTE_LIFE),
            contract: None,
            typed: Some(typed),
        };
        Ok((route, order))
    }

    pub async fn place(&self, network: &NetworkId, body: &str, uid: &str) -> Result<(), SwapError> {
        let body: Value = serde_json::from_str(body).map_err(|_| unreadable())?;
        let path = &self
            .networks
            .get(network)
            .ok_or(SwapError::Unsupported)?
            .path;
        let url = self.url(path, "orders")?;
        let answer = fetch(self.client.post(url).json(&body)).await?;
        if answer.as_str().map(str::to_ascii_lowercase).as_deref() != Some(uid) {
            return Err(SwapError::Untrusted("CoW accepted another order"));
        }
        Ok(())
    }

    pub async fn status(&self, network: &NetworkId, uid: &str) -> Result<Progress, SwapError> {
        let path = &self
            .networks
            .get(network)
            .ok_or(SwapError::Unsupported)?
            .path;
        let valid = uid.len() == 2 + 2 * UID_LEN
            && uid
                .strip_prefix("0x")
                .is_some_and(|digits| bahaa_types::hex::decode(digits).is_some());
        if !valid {
            return Err(unreadable());
        }
        let url = self.url(path, &format!("orders/{uid}"))?;
        let answer = fetch(self.client.get(url)).await?;
        match answer["status"].as_str() {
            Some("open" | "presignaturePending") => Ok(Progress::Swapping),
            Some("fulfilled") => Ok(Progress::Done),
            Some("expired") => Ok(Progress::Failed(
                "the order expired without a fill, nothing left your wallet".to_owned(),
            )),
            Some("cancelled") => Ok(Progress::Failed(
                "the order was cancelled, nothing left your wallet".to_owned(),
            )),
            _ => Err(unreadable()),
        }
    }

    fn url(&self, path: &str, call: &str) -> Result<Url, SwapError> {
        self.base
            .join(&format!("{path}/api/v1/{call}"))
            .map_err(|_| unreadable())
    }
}

fn token(asset: &AssetId) -> Result<[u8; 20], SwapError> {
    match asset.contract() {
        None => Ok(NATIVE),
        Some(Address::Evm(bytes)) => Ok(*bytes),
        Some(_) => Err(SwapError::Unsupported),
    }
}

fn keccak(bytes: &[u8]) -> [u8; 32] {
    Keccak256::digest(bytes).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SETTLEMENT: &str = "9008d19f58aabd9ed0d60971565aa8510560ab41";

    fn domain(chain: u64) -> [u8; 32] {
        let mut domain = Vec::new();
        domain.extend_from_slice(&keccak(DOMAIN_TYPE.as_bytes()));
        domain.extend_from_slice(&keccak(b"Gnosis Protocol"));
        domain.extend_from_slice(&keccak(b"v2"));
        abi::push_uint(&mut domain, U256::from(chain));
        let settlement: [u8; 20] = bahaa_types::hex::decode(SETTLEMENT)
            .unwrap()
            .try_into()
            .unwrap();
        abi::push_address(&mut domain, &settlement);
        keccak(&domain)
    }

    #[test]
    fn the_domain_matches_the_settlement_contract_on_chain() {
        assert_eq!(
            bahaa_types::hex::encode(&domain(1)),
            "c078f884a2676e1345748b1feace7b0abee5d00ecadb6e574dcdd109a63e8943"
        );
        assert_eq!(
            bahaa_types::hex::encode(&domain(56)),
            "0cbb18dfca28d2ceac8c72a17289168e03c1ad121338f5573e3b0c3255207fc7"
        );
        assert_eq!(
            bahaa_types::hex::encode(&keccak(APP_DATA.as_bytes())),
            "b48d38f93eaa084033fc5970bf96e559c33c4cdc07d889ab00b4d63f9590739d"
        );
    }

    #[test]
    fn the_uid_is_the_digest_then_the_owner_then_the_deadline() {
        let typed = TypedOrder {
            chain: 56,
            settlement: [0x90; 20],
            owner: [0x11; 20],
            sell: [0x55; 20],
            buy: NATIVE,
            receiver: [0x11; 20],
            sell_amount: U256::from(5u8),
            buy_amount: U256::from(4u8),
            valid_to: 0x0102_0304,
            quote: None,
        };
        let uid = typed.uid();
        assert_eq!(uid.len(), 2 + 2 * UID_LEN);
        assert!(uid.ends_with(&format!("{}01020304", "11".repeat(20))));
        assert!(uid[2..].starts_with(&bahaa_types::hex::encode(&typed.digest())));
        let body = typed.body(&[7; 65]);
        assert_eq!(body["feeAmount"], "0");
        assert_eq!(body["partiallyFillable"], false);
        assert_eq!(body["receiver"], format!("0x{}", "11".repeat(20)));
    }
}
