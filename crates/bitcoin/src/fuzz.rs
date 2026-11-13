use bahaa_types::hex;
use serde_json::Value;

use crate::{chain, electrum, esplora, header, proof, script, source, tx};

pub fn raw(bytes: &[u8]) {
    tx::fuzz(bytes);
    proof::fuzz(bytes);
    let _ = tx::read_signature(bytes);
    script::fuzz(bytes);
    chain::fuzz(bytes);
}

pub fn json(bytes: &[u8]) {
    let Ok(value) = serde_json::from_slice::<Value>(bytes) else {
        return;
    };
    electrum::fuzz(&value);
    esplora::fuzz(&value);
    if let Some(text) = value.as_str() {
        let _ = source::rate(text, 1_000);
        if let Some(raw) = hex::decode(text) {
            tx::fuzz(&raw);
            let _ = header::time(&raw);
            let _ = header::block_hash(&raw);
        }
    }
}
