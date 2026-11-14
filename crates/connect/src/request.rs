use bahaa_types::Address;
use base64::Engine as _;
use base64::alphabet;
use base64::engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig};
use serde_json::{Map, Value};
use url::Url;

use crate::ConnectError;
use crate::reply::Code;

pub const MAX_MESSAGES: usize = 255;
const MAX_ITEMS: usize = 8;
const MAX_PROOF_PAYLOAD: usize = 1024;
const MAX_CELL: usize = 64 * 1024;
const MAX_TEXT: usize = 64 * 1024;
const MAX_SCHEMA: usize = 16 * 1024;
const MAX_NETWORK: usize = 12;
const MAX_MANIFEST_URL: usize = 2048;
const LENIENT: GeneralPurposeConfig = GeneralPurposeConfig::new()
    .with_decode_allow_trailing_bits(false)
    .with_decode_padding_mode(DecodePaddingMode::Indifferent);
const STANDARD: GeneralPurpose = GeneralPurpose::new(&alphabet::STANDARD, LENIENT);
const URL_SAFE: GeneralPurpose = GeneralPurpose::new(&alphabet::URL_SAFE, LENIENT);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConnectRequest {
    pub manifest: Url,
    pub items: Vec<ConnectItem>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConnectItem {
    Address { network: Option<String> },
    Proof { payload: String },
    Other(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppRequest {
    pub id: u64,
    pub call: Call,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Call {
    SendTransaction(Transaction),
    SignMessage(Transaction),
    SignData(SignData),
    Disconnect,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Transaction {
    pub valid_until: Option<u64>,
    pub network: Option<String>,
    pub from: Option<Address>,
    pub outgoing: Outgoing,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outgoing {
    Messages(Vec<Message>),
    Items(Vec<TransferItem>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Message {
    pub to: Address,
    pub bounce: bool,
    pub testnet: bool,
    pub amount: u128,
    pub payload: Option<Vec<u8>>,
    pub state_init: Option<Vec<u8>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TransferItem {
    Ton(Message),
    Jetton(JettonItem),
    Nft(NftItem),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JettonItem {
    pub master: Address,
    pub destination: Address,
    pub amount: u128,
    pub attach: Option<u128>,
    pub query_id: Option<u64>,
    pub response: Option<Address>,
    pub custom_payload: Option<Vec<u8>>,
    pub forward_amount: Option<u128>,
    pub forward_payload: Option<Vec<u8>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NftItem {
    pub item: Address,
    pub new_owner: Address,
    pub attach: Option<u128>,
    pub query_id: Option<u64>,
    pub response: Option<Address>,
    pub custom_payload: Option<Vec<u8>>,
    pub forward_amount: Option<u128>,
    pub forward_payload: Option<Vec<u8>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SignData {
    pub network: Option<String>,
    pub from: Option<Address>,
    pub content: Content,
    pub payload: Value,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Content {
    Text(String),
    Binary(Vec<u8>),
    Cell { schema: String, cell: Vec<u8> },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Bad {
    pub id: Option<u64>,
    pub code: Code,
    pub reason: &'static str,
}

impl ConnectRequest {
    pub(crate) fn read(text: &str) -> Result<Self, ConnectError> {
        let invalid = ConnectError::Invalid("the connection request can't be read");
        let value: Value = serde_json::from_str(text).map_err(|_| invalid.clone())?;
        let manifest = value
            .get("manifestUrl")
            .and_then(Value::as_str)
            .filter(|url| url.len() <= MAX_MANIFEST_URL)
            .and_then(|url| Url::parse(url).ok())
            .ok_or_else(|| invalid.clone())?;
        if manifest.scheme() != "https" {
            return Err(ConnectError::Invalid("the app's details must be on https"));
        }
        let items = value
            .get("items")
            .and_then(Value::as_array)
            .filter(|items| !items.is_empty() && items.len() <= MAX_ITEMS)
            .ok_or(invalid)?
            .iter()
            .map(connect_item)
            .collect::<Result<Vec<_>, _>>()?;
        if !items
            .iter()
            .any(|item| matches!(item, ConnectItem::Address { .. }))
        {
            return Err(ConnectError::Invalid("the app didn't ask for an address"));
        }
        Ok(Self { manifest, items })
    }
}

fn connect_item(item: &Value) -> Result<ConnectItem, ConnectError> {
    let name = item
        .get("name")
        .and_then(Value::as_str)
        .filter(|name| !name.is_empty() && name.len() <= 64)
        .ok_or(ConnectError::Invalid(
            "the connection request has an unnamed item",
        ))?;
    match name {
        "ton_addr" => Ok(ConnectItem::Address {
            network: optional_network(item.get("network")).map_err(ConnectError::Invalid)?,
        }),
        "ton_proof" => {
            let payload = item
                .get("payload")
                .and_then(Value::as_str)
                .filter(|payload| payload.len() <= MAX_PROOF_PAYLOAD)
                .ok_or(ConnectError::Invalid("the sign-in request can't be read"))?;
            Ok(ConnectItem::Proof {
                payload: payload.to_owned(),
            })
        }
        other => Ok(ConnectItem::Other(other.to_owned())),
    }
}

pub fn read_request(bytes: &[u8]) -> Result<AppRequest, Bad> {
    let bad = |id, reason| Bad {
        id,
        code: Code::BadRequest,
        reason,
    };
    let value: Value =
        serde_json::from_slice(bytes).map_err(|_| bad(None, "the request can't be read"))?;
    let id = value
        .get("id")
        .and_then(|id| match id {
            Value::String(text) => text.parse::<u64>().ok(),
            Value::Number(number) => number.as_u64(),
            _ => None,
        })
        .ok_or_else(|| bad(None, "the request has no id"))?;
    let method = value
        .get("method")
        .and_then(Value::as_str)
        .ok_or_else(|| bad(Some(id), "the request has no method"))?;
    let params = value.get("params").and_then(Value::as_array);
    let first = || -> Result<Value, Bad> {
        let text = params
            .and_then(|params| params.first())
            .and_then(Value::as_str)
            .ok_or_else(|| bad(Some(id), "the request has no parameters"))?;
        serde_json::from_str(text)
            .map_err(|_| bad(Some(id), "the request parameters can't be read"))
    };
    let call = match method {
        "sendTransaction" => {
            Call::SendTransaction(transaction(&first()?).map_err(|reason| bad(Some(id), reason))?)
        }
        "signMessage" => {
            Call::SignMessage(transaction(&first()?).map_err(|reason| bad(Some(id), reason))?)
        }
        "signData" => Call::SignData(sign_data(&first()?).map_err(|reason| bad(Some(id), reason))?),
        "disconnect" => Call::Disconnect,
        _ => {
            return Err(Bad {
                id: Some(id),
                code: Code::NotSupported,
                reason: "this wallet doesn't support this request",
            });
        }
    };
    Ok(AppRequest { id, call })
}

fn transaction(value: &Value) -> Result<Transaction, &'static str> {
    let valid_until = match value.get("valid_until") {
        None | Some(Value::Null) => None,
        Some(until) => Some(number(until).ok_or("the request's deadline can't be read")?),
    };
    let outgoing = match (value.get("messages"), value.get("items")) {
        (Some(messages), None) => Outgoing::Messages(
            list(messages)?
                .iter()
                .map(message)
                .collect::<Result<_, _>>()?,
        ),
        (None, Some(items)) => Outgoing::Items(
            list(items)?
                .iter()
                .map(transfer_item)
                .collect::<Result<_, _>>()?,
        ),
        _ => return Err("the request must hold either messages or items"),
    };
    Ok(Transaction {
        valid_until,
        network: optional_network(value.get("network"))?,
        from: optional_address(value.get("from"))?,
        outgoing,
    })
}

fn list(value: &Value) -> Result<&Vec<Value>, &'static str> {
    value
        .as_array()
        .filter(|list| !list.is_empty() && list.len() <= MAX_MESSAGES)
        .ok_or("the request has no messages or too many")
}

fn message(value: &Value) -> Result<Message, &'static str> {
    let text = value
        .get("address")
        .and_then(Value::as_str)
        .ok_or("a message has no address")?;
    let (to, form) = Address::parse_ton(text)
        .map_err(|_| "a message has an address that isn't a TON address")?;
    let form = form.ok_or("a message address must be in the user-friendly form")?;
    if !no_extra_currency(value) {
        return Err("this wallet doesn't send extra currencies");
    }
    Ok(Message {
        to,
        bounce: form.bounceable,
        testnet: form.testnet,
        amount: amount(value.get("amount")).ok_or("a message amount can't be read")?,
        payload: cell(value.get("payload"))?,
        state_init: cell(value.get("stateInit"))?,
    })
}

fn no_extra_currency(value: &Value) -> bool {
    match value.get("extra_currency") {
        None | Some(Value::Null) => true,
        Some(Value::Object(map)) => map.is_empty(),
        Some(_) => false,
    }
}

fn transfer_item(value: &Value) -> Result<TransferItem, &'static str> {
    let field = |name: &str| value.get(name);
    match field("type").and_then(Value::as_str) {
        Some("ton") => message(value).map(TransferItem::Ton),
        Some("jetton") => Ok(TransferItem::Jetton(JettonItem {
            master: address(field("master")).ok_or("a jetton item has no jetton master")?,
            destination: address(field("destination")).ok_or("a jetton item has no recipient")?,
            amount: amount(field("amount")).ok_or("a jetton item amount can't be read")?,
            attach: optional_amount(field("attachAmount"))?,
            query_id: optional_query(field("queryId"))?,
            response: optional_address(field("responseDestination"))?,
            custom_payload: cell(field("customPayload"))?,
            forward_amount: optional_amount(field("forwardAmount"))?,
            forward_payload: cell(field("forwardPayload"))?,
        })),
        Some("nft") => Ok(TransferItem::Nft(NftItem {
            item: address(field("nftAddress")).ok_or("an NFT item has no NFT address")?,
            new_owner: address(field("newOwner")).ok_or("an NFT item has no new owner")?,
            attach: optional_amount(field("attachAmount"))?,
            query_id: optional_query(field("queryId"))?,
            response: optional_address(field("responseDestination"))?,
            custom_payload: cell(field("customPayload"))?,
            forward_amount: optional_amount(field("forwardAmount"))?,
            forward_payload: cell(field("forwardPayload"))?,
        })),
        _ => Err("the request has an item of an unknown type"),
    }
}

fn sign_data(payload: &Value) -> Result<SignData, &'static str> {
    let text = |name: &str, max: usize| {
        payload
            .get(name)
            .and_then(Value::as_str)
            .filter(|text| text.len() <= max)
    };
    let content = match text("type", 16) {
        Some("text") => Content::Text(
            text("text", MAX_TEXT)
                .ok_or("the text to sign can't be read")?
                .to_owned(),
        ),
        Some("binary") => Content::Binary(
            text("bytes", MAX_TEXT.div_ceil(3) * 4)
                .and_then(decode)
                .filter(|bytes| bytes.len() <= MAX_TEXT)
                .ok_or("the bytes to sign can't be read")?,
        ),
        Some("cell") => Content::Cell {
            schema: text("schema", MAX_SCHEMA)
                .ok_or("the data schema can't be read")?
                .to_owned(),
            cell: cell(payload.get("cell"))?.ok_or("the data to sign can't be read")?,
        },
        _ => return Err("the data to sign has an unknown type"),
    };
    Ok(SignData {
        network: optional_network(payload.get("network"))?,
        from: optional_address(payload.get("from"))?,
        content,
        payload: Value::Object(payload.as_object().cloned().unwrap_or_else(Map::new)),
    })
}

fn optional_network(value: Option<&Value>) -> Result<Option<String>, &'static str> {
    let text = match value {
        None | Some(Value::Null) => return Ok(None),
        Some(Value::String(text)) => text.clone(),
        Some(Value::Number(number)) => number.to_string(),
        Some(_) => return Err("the network can't be read"),
    };
    let digits = text.strip_prefix('-').unwrap_or(&text);
    if text.len() > MAX_NETWORK
        || digits.is_empty()
        || !digits.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err("the network can't be read");
    }
    Ok(Some(text))
}

fn address(value: Option<&Value>) -> Option<Address> {
    let (address, _) = Address::parse_ton(value?.as_str()?).ok()?;
    Some(address)
}

fn optional_address(value: Option<&Value>) -> Result<Option<Address>, &'static str> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(_) => address(value)
            .map(Some)
            .ok_or("an address in the request can't be read"),
    }
}

fn amount(value: Option<&Value>) -> Option<u128> {
    match value? {
        Value::String(text)
            if !text.is_empty()
                && text.len() <= 39
                && text.bytes().all(|byte| byte.is_ascii_digit()) =>
        {
            text.parse().ok()
        }
        Value::Number(number) => number.as_u64().map(u128::from),
        _ => None,
    }
}

fn optional_amount(value: Option<&Value>) -> Result<Option<u128>, &'static str> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(_) => amount(value)
            .map(Some)
            .ok_or("an amount in the request can't be read"),
    }
}

fn optional_query(value: Option<&Value>) -> Result<Option<u64>, &'static str> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(query) => number(query).map(Some).ok_or("a query id can't be read"),
    }
}

fn number(value: &Value) -> Option<u64> {
    match value {
        Value::Number(number) => number.as_u64(),
        Value::String(text)
            if !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit()) =>
        {
            text.parse().ok()
        }
        _ => None,
    }
}

fn cell(value: Option<&Value>) -> Result<Option<Vec<u8>>, &'static str> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) if text.is_empty() => Ok(None),
        Some(Value::String(text)) if text.len() <= MAX_CELL.div_ceil(3) * 4 => decode(text)
            .filter(|bytes| bytes.len() <= MAX_CELL)
            .map(Some)
            .ok_or("a cell in the request isn't valid base64"),
        Some(_) => Err("a cell in the request can't be read"),
    }
}

fn decode(text: &str) -> Option<Vec<u8>> {
    STANDARD
        .decode(text)
        .or_else(|_| URL_SAFE.decode(text))
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    const WALLET: &str = "Ef8AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAADAU";
    const USER: &str = "UQCi1ivY6QriyKIQZrSTwhvf4qXI19qIPe4XAOYo-qMMUEiR";

    fn request(method: &str, params: &Value) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "method": method,
            "params": [params.to_string()],
            "id": "42",
        }))
        .unwrap()
    }

    #[test]
    fn reads_the_spec_send_transaction_example() {
        let bytes = br#"{"method":"sendTransaction","params":["{\"valid_until\":1764424242,\"network\":\"-239\",\"from\":\"Ef8AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAADAU\",\"messages\":[{\"address\":\"Ef8AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAADAU\",\"amount\":\"100000000\"}]}"],"id":"42"}"#;
        let request = read_request(bytes).unwrap();
        assert_eq!(request.id, 42);
        let Call::SendTransaction(transaction) = request.call else {
            panic!("not a transaction");
        };
        assert_eq!(transaction.valid_until, Some(1_764_424_242));
        assert_eq!(transaction.network.as_deref(), Some("-239"));
        let Outgoing::Messages(messages) = transaction.outgoing else {
            panic!("no messages");
        };
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].amount, 100_000_000);
        assert!(messages[0].bounce);
        assert_eq!(messages[0].payload, None);
    }

    #[test]
    fn raw_addresses_extra_currencies_and_both_lists_are_refused() {
        let raw = serde_json::json!({"messages": [{"address": "0:a2d62bd8e90ae2c8a21066b493c21bdfe2a5c8d7da883dee1700e628faa30c50", "amount": "1"}]});
        let bad = read_request(&request("sendTransaction", &raw)).unwrap_err();
        assert_eq!((bad.id, bad.code), (Some(42), Code::BadRequest));
        let extra = serde_json::json!({"messages": [{"address": USER, "amount": "1", "extra_currency": {"100": "5"}}]});
        assert!(read_request(&request("sendTransaction", &extra)).is_err());
        let both = serde_json::json!({"messages": [{"address": USER, "amount": "1"}], "items": []});
        assert!(read_request(&request("sendTransaction", &both)).is_err());
        let unknown = read_request(&request("transferAll", &serde_json::json!({}))).unwrap_err();
        assert_eq!(unknown.code, Code::NotSupported);
        let ids = read_request(br#"{"method":"disconnect","params":[],"id":"x"}"#).unwrap_err();
        assert_eq!(ids.id, None);
    }

    #[test]
    fn structured_items_and_cells_are_read() {
        let items = serde_json::json!({"items": [
            {"type": "ton", "address": USER, "amount": "5", "payload": "te6cckEBAQEAAgAAAEysuc0="},
            {"type": "jetton", "master": WALLET, "destination": USER, "amount": "1000000", "forwardAmount": "1", "queryId": "7"},
            {"type": "nft", "nftAddress": WALLET, "newOwner": USER}
        ]});
        let Call::SignMessage(transaction) =
            read_request(&request("signMessage", &items)).unwrap().call
        else {
            panic!("not a signed message");
        };
        let Outgoing::Items(items) = transaction.outgoing else {
            panic!("no items");
        };
        let TransferItem::Ton(ton) = &items[0] else {
            panic!("not a coin item");
        };
        assert!(!ton.bounce);
        assert_eq!(ton.payload.as_ref().map(Vec::len), Some(17));
        let TransferItem::Jetton(jetton) = &items[1] else {
            panic!("not a jetton item");
        };
        assert_eq!(
            (jetton.amount, jetton.forward_amount, jetton.query_id),
            (1_000_000, Some(1), Some(7))
        );
        assert!(matches!(items[2], TransferItem::Nft(_)));
    }

    #[test]
    fn data_to_sign_keeps_the_original_payload() {
        let text = serde_json::json!({"type": "text", "text": "Sign in to the dApp", "network": "-239", "from": WALLET});
        let Call::SignData(data) = read_request(&request("signData", &text)).unwrap().call else {
            panic!("not sign data");
        };
        assert_eq!(
            data.content,
            Content::Text("Sign in to the dApp".to_owned())
        );
        assert_eq!(data.payload, text);
        let binary = serde_json::json!({"type": "binary", "bytes": "SGVsbG8sIFRPTiE"});
        let Call::SignData(data) = read_request(&request("signData", &binary)).unwrap().call else {
            panic!("not sign data");
        };
        assert_eq!(data.content, Content::Binary(b"Hello, TON!".to_vec()));
        let odd = serde_json::json!({"type": "image", "bytes": "AA=="});
        assert!(read_request(&request("signData", &odd)).is_err());
    }

    #[test]
    fn connect_requests_need_an_https_manifest_and_an_address() {
        let ok = ConnectRequest::read(r#"{"manifestUrl":"https://app.example/tonconnect-manifest.json","items":[{"name":"ton_addr"},{"name":"ton_proof","payload":"nonce"},{"name":"ton_unknown"}]}"#).unwrap();
        assert_eq!(ok.items.len(), 3);
        assert_eq!(
            ok.items[1],
            ConnectItem::Proof {
                payload: "nonce".to_owned()
            }
        );
        assert_eq!(ok.items[2], ConnectItem::Other("ton_unknown".to_owned()));
        assert!(
            ConnectRequest::read(
                r#"{"manifestUrl":"http://app.example/m.json","items":[{"name":"ton_addr"}]}"#
            )
            .is_err()
        );
        assert!(ConnectRequest::read(r#"{"manifestUrl":"https://app.example/m.json","items":[{"name":"ton_proof","payload":"x"}]}"#).is_err());
        assert!(ConnectRequest::read(r#"{"manifestUrl":"https://app.example/m.json","items":[{"name":"ton_addr","network":"main"}]}"#).is_err());
    }
}
