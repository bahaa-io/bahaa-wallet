use bahaa_types::{Address, hex};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde_json::{Value, json};

use crate::request::MAX_MESSAGES;

const APP_NAME: &str = "bahaa-wallet";
const PROTOCOL: u8 = 2;
const ITEM_TYPES: [&str; 3] = ["ton", "jetton", "nft"];
const DATA_TYPES: [&str; 3] = ["text", "binary", "cell"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Code {
    Unknown = 0,
    BadRequest = 1,
    ManifestNotFound = 2,
    ManifestContent = 3,
    UnknownApp = 100,
    Declined = 300,
    NotSupported = 400,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Device<'a> {
    pub version: &'a str,
    pub max_messages: usize,
    pub sign_message: bool,
}

impl Device<'_> {
    fn json(&self) -> Value {
        let send = |name: &str| {
            json!({
                "name": name,
                "maxMessages": self.max_messages.min(MAX_MESSAGES),
                "extraCurrencySupported": false,
                "itemTypes": ITEM_TYPES,
            })
        };
        let mut features = vec![
            json!("SendTransaction"),
            send("SendTransaction"),
            json!({"name": "SignData", "types": DATA_TYPES}),
        ];
        if self.sign_message {
            features.push(send("SignMessage"));
        }
        json!({
            "platform": platform(),
            "appName": APP_NAME,
            "appVersion": self.version,
            "maxProtocolVersion": PROTOCOL,
            "features": features,
        })
    }
}

fn platform() -> &'static str {
    match std::env::consts::OS {
        "macos" => "mac",
        "windows" => "windows",
        _ => "linux",
    }
}

pub fn connected(event: u64, items: &[Value], device: &Device<'_>) -> Vec<u8> {
    bytes(&json!({
        "event": "connect",
        "id": event,
        "payload": {"items": items, "device": device.json()},
    }))
}

pub fn refused(event: u64, code: Code, message: &str) -> Vec<u8> {
    bytes(&json!({
        "event": "connect_error",
        "id": event,
        "payload": {"code": code as u16, "message": message},
    }))
}

pub fn disconnected(event: u64) -> Vec<u8> {
    bytes(&json!({"event": "disconnect", "id": event, "payload": {}}))
}

pub fn answer(id: u64, result: &Value) -> Vec<u8> {
    bytes(&json!({"result": result, "id": id.to_string()}))
}

pub fn failure(id: u64, code: Code, message: &str) -> Vec<u8> {
    bytes(&json!({
        "error": {"code": code as u16, "message": message},
        "id": id.to_string(),
    }))
}

pub fn address_item(
    address: &Address,
    network: &str,
    public_key: &[u8; 32],
    state_init: &[u8],
) -> Value {
    json!({
        "name": "ton_addr",
        "address": address.to_string(),
        "network": network,
        "publicKey": hex::encode(public_key),
        "walletStateInit": STANDARD.encode(state_init),
    })
}

pub fn proof_item(timestamp: u64, domain: &str, signature: &[u8; 64], payload: &str) -> Value {
    json!({
        "name": "ton_proof",
        "proof": {
            "timestamp": timestamp,
            "domain": {"lengthBytes": domain.len(), "value": domain},
            "signature": STANDARD.encode(signature),
            "payload": payload,
        },
    })
}

pub fn unsupported_item(name: &str) -> Value {
    json!({"name": name, "error": {"code": Code::NotSupported as u16, "message": "not supported"}})
}

pub fn signed_data(
    signature: &[u8; 64],
    address: &Address,
    timestamp: u64,
    domain: &str,
    payload: &Value,
) -> Value {
    json!({
        "signature": STANDARD.encode(signature),
        "address": address.to_string(),
        "timestamp": timestamp,
        "domain": domain,
        "payload": payload,
    })
}

fn bytes(value: &Value) -> Vec<u8> {
    value.to_string().into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_and_answers_have_the_spec_shapes() {
        let device = Device {
            version: "0.14.0",
            max_messages: 4,
            sign_message: false,
        };
        let event: Value =
            serde_json::from_slice(&connected(1, &[unsupported_item("ton_proof")], &device))
                .unwrap();
        assert_eq!(event["event"], "connect");
        assert_eq!(event["payload"]["device"]["appName"], APP_NAME);
        assert_eq!(event["payload"]["device"]["maxProtocolVersion"], 2);
        let features = event["payload"]["device"]["features"].as_array().unwrap();
        assert_eq!(features[1]["maxMessages"], 4);
        assert_eq!(features.len(), 3);
        assert_eq!(event["payload"]["items"][0]["error"]["code"], 400);
        let declined: Value = serde_json::from_slice(&failure(42, Code::Declined, "no")).unwrap();
        assert_eq!(
            declined,
            json!({"error": {"code": 300, "message": "no"}, "id": "42"})
        );
        let done: Value =
            serde_json::from_slice(&answer(43, &json!({"internalBoc": "te6"}))).unwrap();
        assert_eq!(done["id"], "43");
        let gone: Value = serde_json::from_slice(&disconnected(5)).unwrap();
        assert_eq!(gone, json!({"event": "disconnect", "id": 5, "payload": {}}));
    }

    #[test]
    fn items_carry_raw_addresses_and_standard_base64() {
        let (address, _) =
            Address::parse_ton("UQCi1ivY6QriyKIQZrSTwhvf4qXI19qIPe4XAOYo-qMMUEiR").unwrap();
        let item = address_item(&address, "-239", &[0xab; 32], &[0xfb, 0xff]);
        assert_eq!(
            item["address"],
            "0:a2d62bd8e90ae2c8a21066b493c21bdfe2a5c8d7da883dee1700e628faa30c50"
        );
        assert_eq!(item["walletStateInit"], "+/8=");
        let proof = proof_item(1_678_206_305, "tonplay.io", &[0; 64], "nonce");
        assert_eq!(proof["proof"]["domain"]["lengthBytes"], 10);
    }
}
