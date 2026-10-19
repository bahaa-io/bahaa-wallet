use serde_json::Value;

use crate::{chain, http, tx};

pub fn raw(data: &[u8]) {
    let _ = tx::parse(data);
    let _ = tx::coins_in(data);
}

pub fn json(data: &[u8]) {
    if let Ok(value) = serde_json::from_slice::<Value>(data) {
        chain::fuzz(&value);
        let _ = http::node_text(&value);
    }
}
