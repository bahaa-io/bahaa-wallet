use std::collections::BTreeMap;

use bahaa_chain::{MAX_MEMO, Wait};
use bahaa_types::{Amount, AssetId, Utc, is_plain_text};
use ed25519_dalek::{Signature, VerifyingKey};
use reqwest::Client;
use ruint::aliases::U256;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use url::Url;

use crate::config::OneClickConfig;
use crate::order::{
    Comment, Order, Progress, Provider, QUOTE_LIFE, Request, SwapError, parse_like, text_of,
};
use crate::reply::{fetch, unreadable};
use crate::route::{Charged, Cost, CostKind, Method, Route};

const SLIPPAGE_BPS: u32 = 100;
const KEY_PREFIX: &str = "ed25519:";
const EXACT_INPUT: &str = "EXACT_INPUT";
const ORIGIN_CHAIN: &str = "ORIGIN_CHAIN";
const DESTINATION_CHAIN: &str = "DESTINATION_CHAIN";

const SIGNED_REQUEST: [&str; 12] = [
    "dry",
    "swapType",
    "slippageTolerance",
    "originAsset",
    "depositType",
    "destinationAsset",
    "amount",
    "refundTo",
    "refundType",
    "recipient",
    "recipientType",
    "deadline",
];
const SIGNED_REQUEST_IF_SET: [&str; 5] = [
    "quoteWaitingTimeMs",
    "referral",
    "virtualChainRecipient",
    "virtualChainRefundRecipient",
    "customRecipientMsg",
];
const SIGNED_QUOTE: [&str; 8] = [
    "amountIn",
    "amountInFormatted",
    "amountInUsd",
    "minAmountIn",
    "amountOut",
    "amountOutFormatted",
    "amountOutUsd",
    "minAmountOut",
];
const SIGNED_ORDER_IF_SET: [&str; 7] = [
    "depositAddress",
    "depositMemo",
    "deadline",
    "timeWhenInactive",
    "timeEstimate",
    "refundFee",
    "withdrawFee",
];

pub struct OneClick {
    client: Client,
    base: Url,
    key: VerifyingKey,
    assets: BTreeMap<AssetId, String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Asked {
    dry: bool,
    swap_type: String,
    slippage_tolerance: u32,
    origin_asset: String,
    deposit_type: String,
    destination_asset: String,
    amount: String,
    refund_to: String,
    refund_type: String,
    recipient: String,
    recipient_type: String,
    deadline: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Answer {
    quote: Quote,
    quote_request: Asked,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Quote {
    amount_in: String,
    amount_out: String,
    min_amount_out: String,
    time_estimate: Option<u64>,
    withdraw_fee: Option<String>,
    deposit_address: Option<String>,
    deposit_memo: Option<String>,
}

impl OneClick {
    pub fn new(config: &OneClickConfig, client: Client) -> Result<Self, SwapError> {
        let key = config
            .key
            .strip_prefix(KEY_PREFIX)
            .and_then(|text| bs58::decode(text).into_vec().ok())
            .and_then(|bytes| <[u8; 32]>::try_from(bytes).ok())
            .and_then(|bytes| VerifyingKey::from_bytes(&bytes).ok())
            .ok_or(SwapError::Untrusted(
                "the NEAR Intents key in the provider list is broken",
            ))?;
        Ok(Self {
            client,
            base: config.url.clone(),
            key,
            assets: config.assets.clone(),
        })
    }

    pub fn covers(&self, request: &Request) -> bool {
        request.sell != request.buy
            && self.assets.contains_key(&request.sell)
            && self.assets.contains_key(&request.buy)
    }

    pub async fn quote(
        &self,
        request: &Request,
        live: bool,
        now: u64,
    ) -> Result<(Route, Option<Order>), SwapError> {
        if !self.covers(request) {
            return Err(SwapError::Unsupported);
        }
        let asked = Asked {
            dry: !live,
            swap_type: EXACT_INPUT.to_owned(),
            slippage_tolerance: SLIPPAGE_BPS,
            origin_asset: self.assets[&request.sell].clone(),
            deposit_type: ORIGIN_CHAIN.to_owned(),
            destination_asset: self.assets[&request.buy].clone(),
            amount: request.amount.units().to_string(),
            refund_to: text_of(&request.from, false),
            refund_type: ORIGIN_CHAIN.to_owned(),
            recipient: text_of(&request.to, false),
            recipient_type: DESTINATION_CHAIN.to_owned(),
            deadline: iso(request.deadline),
        };
        let reply = fetch(self.client.post(self.endpoint("quote")?).json(&asked)).await?;
        if !self.signed(&reply) {
            return Err(SwapError::Untrusted(
                "the quote is not signed by NEAR Intents",
            ));
        }
        let answer: Answer = serde_json::from_value(reply).map_err(|_| unreadable())?;
        if answer.quote_request != asked || answer.quote.amount_in != asked.amount {
            return Err(SwapError::Untrusted("the quote is for another request"));
        }
        let quote = answer.quote;
        let units = |text: &str| bought(text, request.buy_decimals);
        let expected = units(&quote.amount_out)?;
        let minimum = units(&quote.min_amount_out)?;
        let mut costs = Vec::new();
        if let Some(fee) = quote.withdraw_fee.as_deref().map(units).transpose()?
            && !fee.is_zero()
        {
            costs.push(Cost {
                kind: CostKind::ProviderFee,
                asset: request.buy.clone(),
                amount: fee,
                charged: Charged::InQuote,
            });
        }
        let route = Route {
            provider: Provider::OneClick.name(),
            method: Method::Deposit,
            sell: request.sell.clone(),
            amount: request.amount,
            buy: request.buy.clone(),
            expected,
            minimum,
            costs,
            wait: quote.time_estimate.map(Wait::About),
        };
        if !live {
            return Ok((route, None));
        }
        let reference = quote
            .deposit_address
            .ok_or(SwapError::Untrusted("the order has no deposit address"))?;
        let comment = match quote.deposit_memo {
            None => None,
            Some(memo) if is_plain_text(&memo, MAX_MEMO) => Some(Comment::Text(memo)),
            Some(_) => {
                return Err(SwapError::Untrusted(
                    "the order needs a memo this wallet can't send",
                ));
            }
        };
        let order = Order {
            provider: Provider::OneClick,
            deposit: parse_like(&reference, &request.from)?,
            reference,
            comment,
            amount: request.amount,
            expected,
            minimum,
            expires_at: now.saturating_add(QUOTE_LIFE).min(request.deadline),
        };
        Ok((route, Some(order)))
    }

    pub async fn status(&self, reference: &str) -> Result<Progress, SwapError> {
        let mut url = self.endpoint("status")?;
        url.query_pairs_mut()
            .append_pair("depositAddress", reference);
        let reply = fetch(self.client.get(url)).await?;
        Ok(
            match reply
                .get("status")
                .and_then(Value::as_str)
                .ok_or_else(unreadable)?
            {
                "PENDING_DEPOSIT" => Progress::Waiting,
                "KNOWN_DEPOSIT_TX" => Progress::Received,
                "PROCESSING" => Progress::Swapping,
                "SUCCESS" => Progress::Done,
                "REFUNDED" => Progress::Refunded,
                "INCOMPLETE_DEPOSIT" => {
                    Progress::Failed("the deposit was smaller than the order".to_owned())
                }
                "FAILED" => Progress::Failed("the provider could not finish the swap".to_owned()),
                _ => return Err(unreadable()),
            },
        )
    }

    pub async fn submit(&self, reference: &str, transaction: &str) -> Result<(), SwapError> {
        let body = json!({"txHash": transaction, "depositAddress": reference});
        fetch(
            self.client
                .post(self.endpoint("deposit/submit")?)
                .json(&body),
        )
        .await
        .map(|_| ())
    }

    fn endpoint(&self, path: &str) -> Result<Url, SwapError> {
        self.base
            .join(path)
            .map_err(|_| SwapError::Untrusted("the provider address is broken"))
    }

    fn signed(&self, reply: &Value) -> bool {
        let Some(text) = signed_text(reply) else {
            return false;
        };
        let Some(signature) = reply
            .get("signature")
            .and_then(Value::as_str)
            .and_then(|text| text.strip_prefix(KEY_PREFIX))
            .and_then(|text| bs58::decode(text).into_vec().ok())
            .and_then(|bytes| <[u8; 64]>::try_from(bytes).ok())
        else {
            return false;
        };
        let message = bs58::encode(Sha256::digest(text.as_bytes())).into_string();
        self.key
            .verify_strict(message.as_bytes(), &Signature::from_bytes(&signature))
            .is_ok()
    }
}

fn bought(text: &str, decimals: u8) -> Result<Amount, SwapError> {
    text.parse::<U256>()
        .ok()
        .and_then(|units| Amount::new(units, decimals).ok())
        .ok_or_else(unreadable)
}

fn signed_text(reply: &Value) -> Option<String> {
    let request = reply.get("quoteRequest")?.as_object()?;
    let quote = reply.get("quote")?.as_object()?;
    let mut fields: BTreeMap<&str, &Value> = BTreeMap::new();
    for name in SIGNED_REQUEST {
        if let Some(value) = request.get(name) {
            fields.insert(name, value);
        }
    }
    for name in SIGNED_REQUEST_IF_SET {
        if let Some(value) = request.get(name).filter(|value| truthy(value)) {
            fields.insert(name, value);
        }
    }
    for name in SIGNED_QUOTE {
        if let Some(value) = quote.get(name) {
            fields.insert(name, value);
        }
    }
    if request.get("dry") == Some(&Value::Bool(false)) {
        for name in SIGNED_ORDER_IF_SET {
            if let Some(value) = quote.get(name).filter(|value| truthy(value)) {
                fields.insert(name, value);
            }
        }
    }
    fields.insert("timestamp", reply.get("timestamp")?);
    let mut text = String::from("{");
    for (at, (name, value)) in fields.into_iter().enumerate() {
        if at > 0 {
            text.push(',');
        }
        text.push_str(&serde_json::to_string(name).ok()?);
        text.push(':');
        text.push_str(&serde_json::to_string(value).ok()?);
    }
    text.push('}');
    Some(text)
}

fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(flag) => *flag,
        Value::Number(number) => number.as_u64() != Some(0) && number.as_i64() != Some(0),
        Value::String(text) => !text.is_empty(),
        Value::Array(_) | Value::Object(_) => true,
    }
}

fn iso(seconds: u64) -> String {
    let at = Utc::at(seconds);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.000Z",
        at.year, at.month, at.day, at.hour, at.minute, at.second
    )
}
