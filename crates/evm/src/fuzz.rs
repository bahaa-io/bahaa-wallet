use serde_json::Value;

use crate::{chain, rpc, tx};

pub fn raw(data: &[u8]) {
    let _ = tx::parse(data);
    let _ = tx::parse_transfer(data);
    chain::fuzz_input(data);
}

pub fn json(data: &[u8]) {
    if let Ok(value) = serde_json::from_slice::<Value>(data) {
        let _ = rpc::quantity(&value);
        let _ = rpc::classify(
            value["code"].as_i64().unwrap_or_default(),
            value["message"].as_str().unwrap_or_default(),
        );
        chain::fuzz(&value);
    }
}
