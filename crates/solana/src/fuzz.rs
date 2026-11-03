use serde_json::Value;

use crate::{chain, rpc, tx};

pub fn raw(data: &[u8]) {
    tx::fuzz(data);
    chain::fuzz_raw(data);
}

pub fn json(data: &[u8]) {
    if let Ok(value) = serde_json::from_slice::<Value>(data) {
        rpc::fuzz(&value);
        chain::fuzz(&value);
    }
}
