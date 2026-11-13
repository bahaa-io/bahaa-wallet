use std::collections::{BTreeMap, BTreeSet};

use bahaa_chain::{Call, Wait, transfer_data};
use bahaa_types::{Address, Amount, AssetId, ChainFamily, NetworkId, hex};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use reqwest::Client;
use ruint::aliases::U256;
use serde::Deserialize;
use serde_json::{Value, json};
use url::Url;

use crate::config::{RelayChain, RelayConfig, RelayDeposit};
use crate::order::{
    Comment, Contract, Order, Progress, Provider, QUOTE_LIFE, Request, SwapError, parse_like,
    text_of,
};
use crate::reply::{fetch, seconds, unreadable};
use crate::route::{Charged, Cost, CostKind, Method, Route};

const EVM_NATIVE: &str = "0x0000000000000000000000000000000000000000";
const TRON_NATIVE: &str = "T9yD14Nj9j7xAB4dbGeiX9h8unkKHxuWwb";
const TON_NATIVE: &str = "EQAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAM9c";
const SOLANA_NATIVE: &str = "11111111111111111111111111111111";
const TRON_PREFIX: &str = "41";
const REQUEST_ID_LEN: usize = 66;

pub struct Relay {
    client: Client,
    base: Url,
    chains: BTreeMap<NetworkId, RelayChain>,
    assets: BTreeSet<AssetId>,
    contracts: Vec<Address>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Answer {
    steps: Vec<Step>,
    details: Details,
    fees: Option<Fees>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Step {
    id: String,
    kind: String,
    request_id: Option<String>,
    deposit_address: Option<String>,
    items: Vec<Item>,
}

#[derive(Deserialize)]
struct Item {
    data: Value,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Details {
    currency_in: Counted,
    currency_out: Counted,
    time_estimate: Option<serde_json::Number>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Counted {
    amount: String,
    minimum_amount: Option<String>,
}

#[derive(Deserialize)]
struct Fees {
    relayer: Option<Fee>,
}

#[derive(Deserialize)]
struct Fee {
    amount: String,
    currency: Currency,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Currency {
    chain_id: u64,
    address: String,
}

impl Relay {
    pub fn new(config: &RelayConfig, client: Client) -> Result<Self, SwapError> {
        let contracts = config
            .contracts
            .iter()
            .map(|text| Address::parse(ChainFamily::Evm, text))
            .collect::<Result<_, _>>()
            .map_err(|_| SwapError::Untrusted("the built-in Relay contracts are broken"))?;
        Ok(Self {
            client,
            base: config.url.clone(),
            chains: config.chains.clone(),
            assets: config.assets.clone(),
            contracts,
        })
    }

    pub fn covers(&self, request: &Request) -> bool {
        let (Some(origin), Some(_)) = (
            self.chains.get(request.sell.network()),
            self.chains.get(request.buy.network()),
        ) else {
            return false;
        };
        request.sell != request.buy
            && self.assets.contains(&request.sell)
            && self.assets.contains(&request.buy)
            && currency(&request.buy, request.to.family()).is_some()
            && match origin.deposit {
                RelayDeposit::Address => true,
                RelayDeposit::Comment => request.sell.contract().is_none(),
                RelayDeposit::Call => {
                    matches!(request.from, Address::Evm(_))
                        && request.sell.network() != request.buy.network()
                }
                RelayDeposit::Unsupported => false,
            }
    }

    pub async fn quote(&self, request: &Request, now: u64) -> Result<(Route, Order), SwapError> {
        if !self.covers(request) {
            return Err(SwapError::Unsupported);
        }
        let origin = self.chains[request.sell.network()];
        let destination = self.chains[request.buy.network()];
        let (Some(sold), Some(wanted)) = (
            currency(&request.sell, request.from.family()),
            currency(&request.buy, request.to.family()),
        ) else {
            return Err(SwapError::Unsupported);
        };
        let units = request.amount.units().to_string();
        let mut body = json!({
            "user": text_of(&request.from, true),
            "recipient": text_of(&request.to, true),
            "refundTo": text_of(&request.from, true),
            "originChainId": origin.id,
            "destinationChainId": destination.id,
            "originCurrency": sold,
            "destinationCurrency": wanted,
            "amount": units,
            "tradeType": "EXACT_INPUT",
            "useDepositAddress": origin.deposit == RelayDeposit::Address,
        });
        if origin.deposit == RelayDeposit::Address {
            body["strict"] = Value::Bool(true);
        }
        let reply = fetch(self.client.post(self.endpoint("quote")?).json(&body)).await?;
        let answer: Answer = serde_json::from_value(reply).map_err(|_| unreadable())?;
        let call_mode = origin.deposit == RelayDeposit::Call;
        let (approval, step, item, reference) = deposit_step(&answer, call_mode)?;
        if answer.details.currency_in.amount != units {
            return Err(SwapError::Untrusted("the quote is for another amount"));
        }
        let expected = counted(&answer.details.currency_out.amount, request.buy_decimals)?;
        let minimum = counted(
            answer
                .details
                .currency_out
                .minimum_amount
                .as_deref()
                .ok_or_else(unreadable)?,
            request.buy_decimals,
        )?;
        let (deposit, comment, contract) = match origin.deposit {
            RelayDeposit::Address => {
                let deposit = parse_like(
                    step.deposit_address.as_deref().ok_or_else(unreadable)?,
                    &request.from,
                )?;
                plain_transfer(&item.data, request, origin.id, &deposit)?;
                (deposit, None, None)
            }
            RelayDeposit::Comment => {
                let (deposit, body) = ton_message(&item.data, request)?;
                (deposit, Some(Comment::TonBody(body)), None)
            }
            RelayDeposit::Call => {
                let contract = self.contract_call(&item.data, approval, request, origin.id)?;
                (contract.call.to, None, Some(contract))
            }
            RelayDeposit::Unsupported => return Err(SwapError::Unsupported),
        };
        let costs = relayer_fee(answer.fees, request, origin.id, &sold)
            .into_iter()
            .collect();
        let route = Route {
            provider: Provider::Relay.name(),
            method: Method::Deposit,
            sell: request.sell.clone(),
            amount: request.amount,
            buy: request.buy.clone(),
            expected,
            minimum,
            costs,
            wait: answer
                .details
                .time_estimate
                .as_ref()
                .and_then(seconds)
                .map(Wait::About),
        };
        let order = Order {
            provider: Provider::Relay,
            reference,
            deposit,
            comment,
            amount: request.amount,
            expected,
            minimum,
            expires_at: now.saturating_add(QUOTE_LIFE),
            contract,
            typed: None,
        };
        Ok((route, order))
    }

    fn contract_call(
        &self,
        data: &Value,
        approval: Option<&Item>,
        request: &Request,
        chain: u64,
    ) -> Result<Contract, SwapError> {
        let account = |value: &Value| {
            field(value, "to")
                .and_then(|text| Address::parse(ChainFamily::Evm, &text.to_ascii_lowercase()).ok())
        };
        let bytes = |value: &Value| {
            field(value, "data").and_then(|text| hex::decode(text.strip_prefix("0x")?))
        };
        let to = account(data)
            .filter(|to| self.contracts.contains(to))
            .ok_or(SwapError::Untrusted(
                "the provider asked to call an unknown contract",
            ))?;
        let units = request.amount.units();
        let native_in = request.sell.contract().is_none();
        let value = field(data, "value").and_then(|text| text.parse::<U256>().ok());
        let from = field(data, "from")
            .and_then(|text| Address::parse(ChainFamily::Evm, &text.to_ascii_lowercase()).ok());
        let honest = from == Some(request.from)
            && data.get("chainId").and_then(Value::as_u64) == Some(chain)
            && value == Some(if native_in { units } else { U256::ZERO });
        let call = bytes(data)
            .filter(|call| honest && !call.is_empty())
            .ok_or(SwapError::Untrusted(
                "the provider asked for another amount or account",
            ))?;
        let spender = match (request.sell.contract(), approval) {
            (None, None) => None,
            (Some(_), None) => Some(to),
            (Some(token), Some(approval)) => {
                let approved = bytes(&approval.data)
                    .filter(|_| account(&approval.data).as_ref() == Some(token))
                    .and_then(|call| approved_spender(&call, units))
                    .filter(|spender| self.contracts.contains(spender));
                Some(approved.ok_or(SwapError::Untrusted(
                    "the provider asked to approve an unknown contract",
                ))?)
            }
            (None, Some(_)) => return Err(not_a_transfer()),
        };
        Ok(Contract {
            call: Call {
                to,
                value: Amount::new(
                    if native_in { units } else { U256::ZERO },
                    request.native_decimals,
                )
                .map_err(|_| unreadable())?,
                data: call,
            },
            spender,
            ours: false,
            spent: None,
        })
    }

    pub async fn status(&self, reference: &str) -> Result<Progress, SwapError> {
        let mut url = self.endpoint("intents/status/v3")?;
        url.query_pairs_mut().append_pair("requestId", reference);
        let reply = fetch(self.client.get(url)).await?;
        Ok(
            match reply
                .get("status")
                .and_then(Value::as_str)
                .ok_or_else(unreadable)?
            {
                "waiting" => Progress::Waiting,
                "depositing" => Progress::Received,
                "pending" | "submitted" | "delayed" => Progress::Swapping,
                "success" => Progress::Done,
                "refund" | "refunded" => Progress::Refunded,
                "failure" => Progress::Failed("the provider could not finish the swap".to_owned()),
                _ => return Err(unreadable()),
            },
        )
    }

    fn endpoint(&self, path: &str) -> Result<Url, SwapError> {
        self.base
            .join(path)
            .map_err(|_| SwapError::Untrusted("the provider address is broken"))
    }
}

fn field<'a>(value: &'a Value, name: &str) -> Option<&'a str> {
    value.get(name).and_then(Value::as_str)
}

fn approved_spender(call: &[u8], units: U256) -> Option<Address> {
    let approve = [0x09, 0x5e, 0xa7, 0xb3];
    if call.len() != 68 || call[..4] != approve || call[4..16] != [0u8; 12] {
        return None;
    }
    let amount = U256::from_be_slice(&call[36..68]);
    let spender: [u8; 20] = call[16..36].try_into().ok()?;
    (amount == units).then_some(Address::Evm(spender))
}

fn deposit_step(
    answer: &Answer,
    call_mode: bool,
) -> Result<(Option<&Item>, &Step, &Item, String), SwapError> {
    let many = || SwapError::Untrusted("the provider asked for more than one transaction");
    let (approval, step) = match answer.steps.as_slice() {
        [step] => (None, step),
        [first, step] if call_mode && first.id == "approve" && first.kind == "transaction" => {
            let [approval] = first.items.as_slice() else {
                return Err(many());
            };
            (Some(approval), step)
        }
        _ => return Err(many()),
    };
    let [item] = step.items.as_slice() else {
        return Err(many());
    };
    if step.id != "deposit" || step.kind != "transaction" {
        return Err(not_a_transfer());
    }
    let reference = step
        .request_id
        .clone()
        .filter(|id| {
            id.len() == REQUEST_ID_LEN
                && id
                    .strip_prefix("0x")
                    .is_some_and(|digits| hex::decode(digits).is_some())
        })
        .ok_or_else(unreadable)?;
    Ok((approval, step, item, reference))
}

fn relayer_fee(fees: Option<Fees>, request: &Request, origin: u64, sold: &str) -> Option<Cost> {
    let fee = fees?.relayer?;
    if fee.currency.chain_id != origin || !fee.currency.address.eq_ignore_ascii_case(sold) {
        return None;
    }
    Some(Cost {
        kind: CostKind::ProviderFee,
        asset: request.sell.clone(),
        amount: counted(&fee.amount, request.amount.decimals()).ok()?,
        charged: Charged::InQuote,
    })
}

fn currency(asset: &AssetId, family: ChainFamily) -> Option<String> {
    match asset.contract() {
        Some(contract) => Some(text_of(contract, true)),
        None => Some(
            match family {
                ChainFamily::Evm => EVM_NATIVE,
                ChainFamily::Tron => TRON_NATIVE,
                ChainFamily::Ton => TON_NATIVE,
                ChainFamily::Solana => SOLANA_NATIVE,
                ChainFamily::Bitcoin => return None,
            }
            .to_owned(),
        ),
    }
}

fn counted(text: &str, decimals: u8) -> Result<Amount, SwapError> {
    text.parse::<U256>()
        .ok()
        .and_then(|units| Amount::new(units, decimals).ok())
        .ok_or_else(unreadable)
}

fn not_a_transfer() -> SwapError {
    SwapError::Untrusted("the provider asked for something other than a plain transfer")
}

fn plain_transfer(
    data: &Value,
    request: &Request,
    chain: u64,
    deposit: &Address,
) -> Result<(), SwapError> {
    let units = request.amount.units();
    let text =
        |value: &Value, name: &str| value.get(name).and_then(Value::as_str).map(str::to_owned);
    let fine = match (&request.from, deposit, request.sell.contract()) {
        (Address::Evm(_), Address::Evm(to), token) => {
            let account = |name: &str| {
                text(data, name).and_then(|text| {
                    Address::parse(ChainFamily::Evm, &text.to_ascii_lowercase()).ok()
                })
            };
            let value = text(data, "value").and_then(|text| text.parse::<U256>().ok());
            let call = text(data, "data").and_then(|text| hex::decode(text.strip_prefix("0x")?));
            account("from").as_ref() == Some(&request.from)
                && data.get("chainId").and_then(Value::as_u64) == Some(chain)
                && match token {
                    None => {
                        account("to").as_ref() == Some(deposit)
                            && call.is_some_and(|call| call.is_empty())
                            && value == Some(units)
                    }
                    Some(token) => {
                        account("to").as_ref() == Some(token)
                            && call == Some(transfer_data(to, units))
                            && value == Some(U256::ZERO)
                    }
                }
        }
        (Address::Tron(from), Address::Tron(to), token) => {
            let parameter = data.get("parameter").unwrap_or(&Value::Null);
            let tron = |name: &str| {
                text(parameter, name).and_then(|text| hex::decode(text.strip_prefix(TRON_PREFIX)?))
            };
            let kind = text(data, "type");
            tron("owner_address").as_deref() == Some(from.as_slice())
                && match token {
                    None => {
                        kind.as_deref() == Some("TransferContract")
                            && tron("to_address").as_deref() == Some(to.as_slice())
                            && parameter
                                .get("amount")
                                .and_then(Value::as_u64)
                                .map(U256::from)
                                == Some(units)
                    }
                    Some(Address::Tron(contract)) => {
                        kind.as_deref() == Some("TriggerSmartContract")
                            && tron("contract_address").as_deref() == Some(contract.as_slice())
                            && text(parameter, "data").and_then(|text| hex::decode(&text))
                                == Some(transfer_data(to, units))
                            && parameter
                                .get("call_value")
                                .is_none_or(|value| value.as_u64() == Some(0))
                    }
                    Some(_) => false,
                }
        }
        _ => false,
    };
    fine.then_some(()).ok_or_else(not_a_transfer)
}

fn ton_message(data: &Value, request: &Request) -> Result<(Address, Vec<u8>), SwapError> {
    let Some([message]) = data
        .get("messages")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
    else {
        return Err(not_a_transfer());
    };
    let field = |name: &str| message.get(name).and_then(Value::as_str);
    let from = field("from").and_then(|text| Address::parse(ChainFamily::Ton, text).ok());
    if request.sell.contract().is_some()
        || from.as_ref().is_some_and(|from| from != &request.from)
        || field("value") != Some(request.amount.units().to_string().as_str())
    {
        return Err(not_a_transfer());
    }
    let deposit = parse_like(field("to").ok_or_else(not_a_transfer)?, &request.from)?;
    let body = field("body")
        .and_then(|body| STANDARD.decode(body).ok())
        .ok_or_else(unreadable)?;
    Ok((deposit, body))
}

#[cfg(fuzzing)]
pub(crate) fn fuzz(data: &[u8]) {
    let _ = approved_spender(data, U256::from(1u8));
    if let Some(units) = data
        .get(4..)
        .and_then(|args| bahaa_chain::abi::uint(args, 1))
    {
        let _ = approved_spender(data, units);
    }
}

#[cfg(fuzzing)]
pub(crate) fn fuzz_reply(value: &Value) {
    let (Ok(sell), Ok(buy), Ok(amount)) = (
        "ton".parse::<AssetId>(),
        "base".parse::<AssetId>(),
        Amount::new(U256::from(1_000_000_000u64), 9),
    ) else {
        return;
    };
    let request = Request {
        sell,
        amount,
        buy,
        buy_decimals: 18,
        native_decimals: 9,
        from: Address::Ton {
            workchain: 0,
            hash: [1; 32],
        },
        to: Address::Evm([2; 20]),
        deadline: 0,
    };
    let _ = ton_message(value, &request);
}
